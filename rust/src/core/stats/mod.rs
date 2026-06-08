mod format;
mod io;
mod model;

pub use format::*;
pub use model::*;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// (current_state, baseline_from_disk, last_flush_time)
static STATS_BUFFER: Mutex<Option<(StatsStore, StatsStore, Instant)>> = Mutex::new(None);

const FLUSH_INTERVAL_SECS: u64 = 30;

pub fn load() -> StatsStore {
    let guard = STATS_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((ref current, ref baseline, _)) = *guard {
        let disk = io::load_from_disk();
        return io::apply_deltas(&disk, current, baseline);
    }
    drop(guard);
    io::load_from_disk()
}

pub fn save(store: &StatsStore) {
    io::locked_write(store);
}

fn maybe_flush(store: &mut StatsStore, baseline: &mut StatsStore, last_flush: &mut Instant) {
    if last_flush.elapsed().as_secs() >= FLUSH_INTERVAL_SECS {
        let merged = io::merge_and_save(store, baseline);
        *store = merged.clone();
        *baseline = merged;
        *last_flush = Instant::now();
    }
}

pub fn flush() {
    let mut guard = STATS_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((ref mut store, ref mut baseline, ref mut last_flush)) = *guard {
        let merged = io::merge_and_save(store, baseline);
        *store = merged.clone();
        *baseline = merged;
        *last_flush = Instant::now();
    }
}

/// Adjust saved tokens after post-processing (terse, hints) changed the output size.
/// Positive delta = savings were over-reported, negative = under-reported.
pub fn adjust_savings(command: &str, over_report_delta: i64) {
    let mut guard = STATS_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((store, _, _)) = guard.as_mut() else {
        return;
    };
    if over_report_delta > 0 {
        let adj = over_report_delta as u64;
        store.total_output_tokens = store.total_output_tokens.saturating_add(adj);
        if let Some(cmd) = store.commands.get_mut(command) {
            cmd.output_tokens = cmd.output_tokens.saturating_add(adj);
        }
    } else {
        let adj = over_report_delta.unsigned_abs();
        store.total_output_tokens = store.total_output_tokens.saturating_sub(adj);
        if let Some(cmd) = store.commands.get_mut(command) {
            cmd.output_tokens = cmd.output_tokens.saturating_sub(adj);
        }
    }
}

pub fn record(command: &str, input_tokens: usize, output_tokens: usize) {
    let mut guard = STATS_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        let disk = io::load_from_disk();
        *guard = Some((disk.clone(), disk, Instant::now()));
    }
    let Some((store, baseline, last_flush)) = guard.as_mut() else {
        return;
    };

    let is_first_command = store.total_commands == baseline.total_commands;
    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let timestamp = now.to_rfc3339();

    store.total_commands = store.total_commands.saturating_add(1);
    store.total_input_tokens = store.total_input_tokens.saturating_add(input_tokens as u64);
    store.total_output_tokens = store
        .total_output_tokens
        .saturating_add(output_tokens as u64);

    if store.first_use.is_none() {
        store.first_use = Some(timestamp.clone());
    }
    store.last_use = Some(timestamp);

    let cmd_key = format::normalize_command(command);
    let entry = store.commands.entry(cmd_key).or_default();
    entry.count = entry.count.saturating_add(1);
    entry.input_tokens = entry.input_tokens.saturating_add(input_tokens as u64);
    entry.output_tokens = entry.output_tokens.saturating_add(output_tokens as u64);

    let current_version = env!("CARGO_PKG_VERSION").to_string();
    if let Some(day) = store.daily.last_mut() {
        if day.date == today {
            day.commands = day.commands.saturating_add(1);
            day.input_tokens = day.input_tokens.saturating_add(input_tokens as u64);
            day.output_tokens = day.output_tokens.saturating_add(output_tokens as u64);
            // Stamp the running version so a mid-day update attributes the day
            // to the release in use for its latest activity (#307).
            day.version = current_version;
        } else {
            store.daily.push(DayStats {
                date: today,
                commands: 1,
                input_tokens: input_tokens as u64,
                output_tokens: output_tokens as u64,
                version: current_version,
            });
        }
    } else {
        store.daily.push(DayStats {
            date: today,
            commands: 1,
            input_tokens: input_tokens as u64,
            output_tokens: output_tokens as u64,
            version: current_version,
        });
    }

    if store.daily.len() > 90 {
        store.daily.drain(..store.daily.len() - 90);
    }

    if is_first_command {
        let merged = io::merge_and_save(store, baseline);
        *store = merged.clone();
        *baseline = merged;
        *last_flush = Instant::now();
    } else {
        maybe_flush(store, baseline, last_flush);
    }
}

pub fn reset_cep() {
    let mut guard = STATS_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut store = io::load_from_disk();
    store.cep = CepStats::default();
    io::locked_write(&store);
    *guard = Some((store.clone(), store, Instant::now()));
}

pub fn reset_all() {
    let mut guard = STATS_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let store = StatsStore::default();
    io::locked_write(&store);
    *guard = Some((store.clone(), store, Instant::now()));
    crate::core::heatmap::reset();
}

pub fn load_stats() -> GainSummary {
    let store = load();
    let input_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens);
    GainSummary {
        total_saved: input_saved,
        total_calls: store.total_commands,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn record_cep_session(
    score: u32,
    cache_hits: u64,
    cache_reads: u64,
    tokens_original: u64,
    tokens_compressed: u64,
    modes: &HashMap<String, u64>,
    tool_calls: u64,
    complexity: &str,
) {
    let mut guard = STATS_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        let disk = io::load_from_disk();
        *guard = Some((disk.clone(), disk, Instant::now()));
    }
    let Some((store, baseline, last_flush)) = guard.as_mut() else {
        return;
    };

    apply_cep_snapshot(
        &mut store.cep,
        std::process::id(),
        score,
        cache_hits,
        cache_reads,
        tokens_original,
        tokens_compressed,
        modes,
        tool_calls,
        complexity,
    );

    maybe_flush(store, baseline, last_flush);
}

/// Fold one CEP snapshot into `cep`. Pure (no globals, no I/O) so the
/// delta/aggregation rules are unit-testable in isolation.
///
/// `cache_hits`, `cache_reads`, `tokens_original` and `tokens_compressed` arrive
/// as **cumulative per-process** counters. For repeated snapshots within the same
/// PID only the delta since the previous snapshot is added, so the lifetime
/// totals keep tracking cache activity instead of freezing at the first
/// checkpoint's value (#361). A new PID starts a fresh session and seeds the
/// cumulative baselines.
#[allow(clippy::too_many_arguments)]
fn apply_cep_snapshot(
    cep: &mut CepStats,
    pid: u32,
    score: u32,
    cache_hits: u64,
    cache_reads: u64,
    tokens_original: u64,
    tokens_compressed: u64,
    modes: &HashMap<String, u64>,
    tool_calls: u64,
    complexity: &str,
) {
    let prev_original = cep.last_session_original.unwrap_or(0);
    let prev_compressed = cep.last_session_compressed.unwrap_or(0);
    let prev_cache_hits = cep.last_session_cache_hits.unwrap_or(0);
    let prev_cache_reads = cep.last_session_cache_reads.unwrap_or(0);
    let is_same_session = cep.last_session_pid == Some(pid);

    if is_same_session {
        cep.total_tokens_original += tokens_original.saturating_sub(prev_original);
        cep.total_tokens_compressed += tokens_compressed.saturating_sub(prev_compressed);
        cep.total_cache_hits += cache_hits.saturating_sub(prev_cache_hits);
        cep.total_cache_reads += cache_reads.saturating_sub(prev_cache_reads);
    } else {
        cep.sessions += 1;
        cep.total_cache_hits += cache_hits;
        cep.total_cache_reads += cache_reads;
        cep.total_tokens_original += tokens_original;
        cep.total_tokens_compressed += tokens_compressed;

        for (mode, count) in modes {
            *cep.modes.entry(mode.clone()).or_insert(0) += count;
        }
    }

    cep.last_session_pid = Some(pid);
    cep.last_session_original = Some(tokens_original);
    cep.last_session_compressed = Some(tokens_compressed);
    cep.last_session_cache_hits = Some(cache_hits);
    cep.last_session_cache_reads = Some(cache_reads);

    let cache_hit_rate = if cache_reads > 0 {
        (cache_hits as f64 / cache_reads as f64 * 100.0).round() as u32
    } else {
        0
    };

    let compression_rate = if tokens_original > 0 {
        ((tokens_original - tokens_compressed) as f64 / tokens_original as f64 * 100.0).round()
            as u32
    } else {
        0
    };

    let total_modes = 6u32;
    let mode_diversity =
        ((modes.len() as f64 / total_modes as f64).min(1.0) * 100.0).round() as u32;

    let tokens_saved = tokens_original.saturating_sub(tokens_compressed);

    cep.scores.push(CepSessionSnapshot {
        timestamp: chrono::Local::now().to_rfc3339(),
        score,
        cache_hit_rate,
        mode_diversity,
        compression_rate,
        tool_calls,
        tokens_saved,
        complexity: complexity.to_string(),
    });

    if cep.scores.len() > 100 {
        cep.scores.drain(..cep.scores.len() - 100);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store(commands: u64, input: u64, output: u64) -> StatsStore {
        StatsStore {
            total_commands: commands,
            total_input_tokens: input,
            total_output_tokens: output,
            ..Default::default()
        }
    }

    #[test]
    fn apply_deltas_merges_mcp_and_shell() {
        let baseline = make_store(0, 0, 0);
        let mut current = make_store(0, 0, 0);
        current.total_commands = 5;
        current.total_input_tokens = 1000;
        current.total_output_tokens = 200;
        current.commands.insert(
            "ctx_read".to_string(),
            CommandStats {
                count: 5,
                input_tokens: 1000,
                output_tokens: 200,
            },
        );

        let mut disk = make_store(20, 500, 490);
        disk.commands.insert(
            "echo".to_string(),
            CommandStats {
                count: 20,
                input_tokens: 500,
                output_tokens: 490,
            },
        );

        let merged = io::apply_deltas(&disk, &current, &baseline);

        assert_eq!(merged.total_commands, 25);
        assert_eq!(merged.total_input_tokens, 1500);
        assert_eq!(merged.total_output_tokens, 690);
        assert_eq!(merged.commands["ctx_read"].count, 5);
        assert_eq!(merged.commands["echo"].count, 20);
    }

    #[test]
    fn apply_deltas_incremental_flush() {
        let baseline = make_store(10, 200, 100);
        let current = make_store(15, 700, 300);

        let disk = make_store(30, 600, 500);

        let merged = io::apply_deltas(&disk, &current, &baseline);

        assert_eq!(merged.total_commands, 35);
        assert_eq!(merged.total_input_tokens, 1100);
        assert_eq!(merged.total_output_tokens, 700);
    }

    #[test]
    fn apply_deltas_preserves_disk_commands() {
        let baseline = make_store(0, 0, 0);
        let mut current = make_store(2, 100, 50);
        current.commands.insert(
            "ctx_read".to_string(),
            CommandStats {
                count: 2,
                input_tokens: 100,
                output_tokens: 50,
            },
        );

        let mut disk = make_store(10, 300, 280);
        disk.commands.insert(
            "echo".to_string(),
            CommandStats {
                count: 8,
                input_tokens: 200,
                output_tokens: 200,
            },
        );
        disk.commands.insert(
            "ctx_read".to_string(),
            CommandStats {
                count: 3,
                input_tokens: 150,
                output_tokens: 80,
            },
        );

        let merged = io::apply_deltas(&disk, &current, &baseline);

        assert_eq!(merged.commands["echo"].count, 8);
        assert_eq!(merged.commands["ctx_read"].count, 5);
        assert_eq!(merged.commands["ctx_read"].input_tokens, 250);
    }

    #[test]
    fn merge_daily_combines_same_date() {
        let baseline_daily = vec![];
        let current_daily = vec![DayStats {
            date: "2026-04-18".to_string(),
            commands: 5,
            input_tokens: 1000,
            output_tokens: 200,
            version: "3.7.0".to_string(),
        }];
        let mut merged_daily = vec![DayStats {
            date: "2026-04-18".to_string(),
            commands: 20,
            input_tokens: 500,
            output_tokens: 490,
            version: String::new(),
        }];

        io::merge_daily(&mut merged_daily, &current_daily, &baseline_daily);

        assert_eq!(merged_daily.len(), 1);
        assert_eq!(merged_daily[0].commands, 25);
        assert_eq!(merged_daily[0].input_tokens, 1500);
        // #307: the most recent known version is carried into the merge.
        assert_eq!(merged_daily[0].version, "3.7.0");
    }

    #[test]
    fn cep_snapshot_seeds_new_session() {
        let mut cep = CepStats::default();
        let modes = HashMap::from([("full".to_string(), 3)]);
        apply_cep_snapshot(&mut cep, 100, 80, 5, 10, 1000, 200, &modes, 4, "Medium");
        assert_eq!(cep.sessions, 1);
        assert_eq!(cep.total_cache_hits, 5);
        assert_eq!(cep.total_cache_reads, 10);
        assert_eq!(cep.total_tokens_original, 1000);
        assert_eq!(cep.total_tokens_compressed, 200);
        assert_eq!(cep.scores.len(), 1);
    }

    #[test]
    fn cep_snapshot_same_pid_accumulates_cache_delta() {
        // #361: repeated snapshots within one process must keep counting cache
        // hits/reads (cumulative counters → add the delta), not freeze at the
        // first checkpoint's value while only tokens advanced.
        let mut cep = CepStats::default();
        let modes = HashMap::new();
        apply_cep_snapshot(&mut cep, 100, 80, 2, 4, 500, 100, &modes, 2, "Low");
        // Same PID, cumulative counters grew: hits 2→9, reads 4→20.
        apply_cep_snapshot(&mut cep, 100, 85, 9, 20, 1500, 300, &modes, 6, "Low");

        assert_eq!(cep.sessions, 1, "same PID must not start a new session");
        assert_eq!(cep.total_cache_hits, 9, "2 + delta(9-2)");
        assert_eq!(cep.total_cache_reads, 20, "4 + delta(20-4)");
        assert_eq!(cep.total_tokens_original, 1500);
        assert_eq!(cep.total_tokens_compressed, 300);
        assert_eq!(cep.scores.len(), 2);
    }

    #[test]
    fn cep_snapshot_new_pid_starts_fresh_session() {
        let mut cep = CepStats::default();
        let modes = HashMap::new();
        apply_cep_snapshot(&mut cep, 100, 80, 5, 10, 1000, 200, &modes, 4, "Medium");
        apply_cep_snapshot(&mut cep, 200, 80, 3, 6, 800, 150, &modes, 4, "Medium");
        assert_eq!(cep.sessions, 2);
        assert_eq!(
            cep.total_cache_hits, 8,
            "5 (session 1) + 3 (session 2, fresh)"
        );
        assert_eq!(cep.total_cache_reads, 16);
    }

    #[test]
    fn format_pct_1dp_normal() {
        assert_eq!(format::format_pct_1dp(50.0), "50.0%");
        assert_eq!(format::format_pct_1dp(100.0), "100.0%");
        assert_eq!(format::format_pct_1dp(33.333), "33.3%");
    }

    #[test]
    fn format_pct_1dp_small_values() {
        assert_eq!(format::format_pct_1dp(0.0), "0.0%");
        assert_eq!(format::format_pct_1dp(0.05), "<0.1%");
        assert_eq!(format::format_pct_1dp(0.09), "<0.1%");
        assert_eq!(format::format_pct_1dp(0.1), "0.1%");
        assert_eq!(format::format_pct_1dp(0.5), "0.5%");
    }
}
