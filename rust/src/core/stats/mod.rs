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

const FLUSH_INTERVAL_SECS: u64 = 2;

/// Daily savings history retained on disk (~10 years of active days). This is a
/// storage/sync safety bound, NOT a display limit: all-time token totals are
/// unbounded, and the cumulative-savings chart baselines any pre-window savings
/// so it always reaches the true all-time total regardless of this window.
pub(super) const MAX_DAILY_HISTORY_DAYS: usize = 3650;

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

/// Loads stats for **display**, summing across every auto-resolved data dir that
/// holds a `stats.json` (#500). When the MCP server process and the CLI resolve
/// different XDG dirs — the documented #408/#414 split, common when an agent host
/// (e.g. a containerised Hermes) launches the MCP server with a different `HOME`
/// or `XDG_*` than the user's shell — the bulk of the savings can land in a
/// sibling tree. Reading only the primary dir then makes `gain` report `0` while
/// the real data sits one directory over. Folding the siblings in keeps the
/// headline honest regardless of which process wrote where.
///
/// Safe by construction:
/// - **No-op without a split** — when only the primary dir has stats (the
///   overwhelmingly common case) the result equals [`load`].
/// - **Respects an explicit pin** — when `LEAN_CTX_DATA_DIR` is set the user has
///   chosen exactly one dir, so nothing is auto-merged.
/// - **Read-only** — never writes back; recording still targets the primary dir.
pub fn load_for_display() -> StatsStore {
    let primary = load();
    // An explicit override means "use exactly this dir" — never auto-merge.
    if std::env::var_os("LEAN_CTX_DATA_DIR").is_some() {
        return primary;
    }
    let primary_dir = crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .and_then(|p| std::fs::canonicalize(&p).ok());
    let siblings: Vec<StatsStore> = crate::core::data_dir::all_data_dirs_with_stats()
        .into_iter()
        .filter(|d| std::fs::canonicalize(d).ok() != primary_dir)
        .map(|d| io::load_from_dir(&d))
        .collect();
    aggregate_for_display(primary, &siblings)
}

/// Folds sibling-dir stores into the primary for display. Pure (no I/O, no
/// globals) so the cross-dir summation is unit-testable. Reuses
/// [`io::apply_deltas`] with a zero baseline so each sibling store is added in
/// full (delta-from-empty == the whole store).
fn aggregate_for_display(primary: StatsStore, siblings: &[StatsStore]) -> StatsStore {
    let zero = StatsStore::default();
    siblings
        .iter()
        .fold(primary, |acc, other| io::apply_deltas(&acc, other, &zero))
}

pub fn save(store: &StatsStore) {
    io::locked_write(store);
}

/// Additively merge two stores (used by data consolidation to combine
/// split stats.json files without losing data from either side).
pub fn merge_additive(base: &StatsStore, other: &StatsStore) -> StatsStore {
    let zero = StatsStore::default();
    io::apply_deltas(base, other, &zero)
}

fn maybe_flush(store: &mut StatsStore, baseline: &mut StatsStore, last_flush: &mut Instant) {
    if last_flush.elapsed().as_secs() >= FLUSH_INTERVAL_SECS
        && let Some(merged) = io::merge_and_save(store, baseline)
    {
        *store = merged.clone();
        *baseline = merged;
        *last_flush = Instant::now();
    }
}

pub fn flush() {
    let mut guard = STATS_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((ref mut store, ref mut baseline, ref mut last_flush)) = *guard
        && let Some(merged) = io::merge_and_save(store, baseline)
    {
        *store = merged.clone();
        *baseline = merged;
        *last_flush = Instant::now();
    }
}

/// Debounced flush: persists at most once per FLUSH_INTERVAL_SECS.
/// Call from post_dispatch on every tool call to bound data loss
/// on abrupt MCP termination to at most 2 seconds of events.
pub fn flush_if_due() {
    let due = {
        let guard = STATS_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.as_ref() {
            Some((_, _, last_flush)) => last_flush.elapsed().as_secs() >= FLUSH_INTERVAL_SECS,
            None => false,
        }
    };
    if due {
        flush();
    }
}

/// Adjust saved tokens after post-processing (terse, hints) changed the output size.
/// Positive delta = savings were over-reported, negative = under-reported.
///
/// Persist immediately: this adjustment follows a durable `record()` and must not
/// be lost when a short-lived MCP process exits before the periodic CEP flush.
pub fn adjust_savings(command: &str, over_report_delta: i64) {
    let mut guard = STATS_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((store, baseline, last_flush)) = guard.as_mut() else {
        return;
    };
    apply_adjust(store, command, over_report_delta);
    if let Some(merged) = io::merge_and_save(store, baseline) {
        *store = merged.clone();
        *baseline = merged;
        *last_flush = Instant::now();
    }
    drop(guard);
    record_for_project(|project| apply_adjust(project, command, over_report_delta));
}

fn apply_adjust(store: &mut StatsStore, command: &str, over_report_delta: i64) {
    let cmd_key = format::normalize_command(command);
    let stream_tracked = classify_command(&cmd_key) == TrafficClass::Compressible;
    if over_report_delta > 0 {
        let adj = over_report_delta as u64;
        store.total_output_tokens = store.total_output_tokens.saturating_add(adj);
        if let Some(cmd) = store.commands.get_mut(&cmd_key) {
            cmd.output_tokens = cmd.output_tokens.saturating_add(adj);
        }
        if stream_tracked {
            store.first_inject_tokens_saved = store.first_inject_tokens_saved.saturating_sub(adj);
            store.active_tool_result_tokens_saved =
                store.active_tool_result_tokens_saved.saturating_sub(adj);
        }
    } else {
        let adj = over_report_delta.unsigned_abs();
        store.total_output_tokens = store.total_output_tokens.saturating_sub(adj);
        if let Some(cmd) = store.commands.get_mut(&cmd_key) {
            cmd.output_tokens = cmd.output_tokens.saturating_sub(adj);
        }
        if stream_tracked {
            store.first_inject_tokens_saved = store.first_inject_tokens_saved.saturating_add(adj);
            if store.last_tool_result_turn > 0 {
                store.active_tool_result_tokens_saved =
                    store.active_tool_result_tokens_saved.saturating_add(adj);
            }
        }
    }
}

pub fn record(command: &str, input_tokens: usize, output_tokens: usize) {
    record_at_turn(command, input_tokens, output_tokens, 0);
}

pub fn record_reread(tokens_saved: usize) {
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
    store.reread_tokens_saved = store
        .reread_tokens_saved
        .saturating_add(tokens_saved as u64);
    if let Some(merged) = io::merge_and_save(store, baseline) {
        *store = merged.clone();
        *baseline = merged;
        *last_flush = Instant::now();
    }
    drop(guard);
    record_for_project(|project| {
        project.reread_tokens_saved = project
            .reread_tokens_saved
            .saturating_add(tokens_saved as u64);
    });
}

/// Records a tool result against an observed provider turn. `turn == 0` keeps
/// daemon-free callers honest: first injection is known, re-read count is not.
pub fn record_at_turn(command: &str, input_tokens: usize, output_tokens: usize, turn: u64) {
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

    apply_record(store, command, input_tokens, output_tokens, turn);

    // MCP stdio servers are routinely terminated without a graceful shutdown.
    // Delaying this write made the append-only ledger survive while aggregate
    // stats disappeared, so persist every completed accounting event.
    if let Some(merged) = io::merge_and_save(store, baseline) {
        *store = merged.clone();
        *baseline = merged;
        *last_flush = Instant::now();
    }
    drop(guard);
    record_for_project(|project| {
        apply_record(project, command, input_tokens, output_tokens, turn);
    });
}

/// Books one event into the store of the project being worked on as well;
/// `stats.json` stays the device total.
fn record_for_project(apply: impl FnOnce(&mut StatsStore)) {
    if let Some(key) = crate::core::savings_ledger::project_scope::current_project_key() {
        io::update_project(&key, apply);
    }
}

/// The usage counters of one project: the stores of its project keys, summed.
pub fn load_for_project(scope: &crate::core::savings_ledger::ProjectScope) -> StatsStore {
    let zero = StatsStore::default();
    let hashes = scope.repo_hashes();
    io::load_projects(hashes.iter().map(String::as_str))
        .iter()
        .fold(StatsStore::default(), |acc, other| {
            io::apply_deltas(&acc, other, &zero)
        })
}

/// Every project store by its project key (for the device view's split).
pub fn load_all_projects() -> Vec<(String, StatsStore)> {
    io::project_keys()
        .into_iter()
        .filter_map(|key| {
            let store = io::load_projects(std::iter::once(key.as_str())).pop()?;
            Some((key, store))
        })
        .collect()
}

/// Counts one tool result into `store` (totals, per command, per day).
fn apply_record(
    store: &mut StatsStore,
    command: &str,
    input_tokens: usize,
    output_tokens: usize,
    turn: u64,
) {
    // Tool-call accounting is durable per event, matching the savings ledger.
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
    if classify_command(&cmd_key) == TrafficClass::Compressible {
        let saved = input_tokens.saturating_sub(output_tokens) as u64;
        store.record_tool_result_savings(saved, turn);
    }
    store
        .command_classes
        .insert(cmd_key.clone(), classify_command(&cmd_key));
    let is_compressible = classify_command(&cmd_key) == TrafficClass::Compressible;
    let entry = store.commands.entry(cmd_key).or_default();
    entry.count = entry.count.saturating_add(1);
    entry.input_tokens = entry.input_tokens.saturating_add(input_tokens as u64);
    entry.output_tokens = entry.output_tokens.saturating_add(output_tokens as u64);

    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let ci = if is_compressible {
        input_tokens as u64
    } else {
        0
    };
    let co = if is_compressible {
        output_tokens as u64
    } else {
        0
    };
    if let Some(day) = store.daily.last_mut() {
        if day.date == today {
            day.commands = day.commands.saturating_add(1);
            day.input_tokens = day.input_tokens.saturating_add(input_tokens as u64);
            day.output_tokens = day.output_tokens.saturating_add(output_tokens as u64);
            day.compressible_input_tokens = day.compressible_input_tokens.saturating_add(ci);
            day.compressible_output_tokens = day.compressible_output_tokens.saturating_add(co);
            day.version = current_version;
        } else {
            store.daily.push(DayStats {
                date: today,
                commands: 1,
                input_tokens: input_tokens as u64,
                output_tokens: output_tokens as u64,
                compressible_input_tokens: ci,
                compressible_output_tokens: co,
                version: current_version,
            });
        }
    } else {
        store.daily.push(DayStats {
            date: today,
            commands: 1,
            input_tokens: input_tokens as u64,
            output_tokens: output_tokens as u64,
            compressible_input_tokens: ci,
            compressible_output_tokens: co,
            version: current_version,
        });
    }

    if store.daily.len() > MAX_DAILY_HISTORY_DAYS {
        store
            .daily
            .drain(..store.daily.len() - MAX_DAILY_HISTORY_DAYS);
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
    drop(guard);
    record_for_project(|project| {
        apply_cep_snapshot(
            &mut project.cep,
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
    });
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

    /// #706: a corrupt `stats.json` must never be silently replaced by an
    /// empty store. The loader quarantines the bytes to `stats.json.corrupt`
    /// (recoverable), and an existing quarantine is never overwritten — the
    /// older copy is the one closest to the lost history.
    #[test]
    fn corrupt_stats_file_is_quarantined_not_silently_reset() {
        let dir = crate::core::data_dir::isolated_data_dir();
        let stats_path = dir.path().join("stats.json");
        let quarantine = dir.path().join("stats.json.corrupt");
        let truncated = r#"{"total_commands": 15677, "total_input_tok"#;
        std::fs::write(&stats_path, truncated).unwrap();

        let loaded = io::load_from_disk();
        assert_eq!(loaded.total_commands, 0, "fresh store after corruption");
        assert!(
            !stats_path.exists(),
            "corrupt file must be moved aside, not left to be overwritten"
        );
        assert_eq!(
            std::fs::read_to_string(&quarantine).unwrap(),
            truncated,
            "quarantine preserves the corrupt bytes verbatim for recovery"
        );

        // A second corruption must NOT clobber the first quarantine.
        std::fs::write(&stats_path, "{ newer corruption").unwrap();
        let loaded = io::load_from_disk();
        assert_eq!(loaded.total_commands, 0);
        assert_eq!(
            std::fs::read_to_string(&quarantine).unwrap(),
            truncated,
            "the OLDER quarantine wins — it is closest to the lost history"
        );

        // And a healthy file still loads normally.
        let healthy = make_store(42, 9000, 1000);
        std::fs::write(&stats_path, serde_json::to_string(&healthy).unwrap()).unwrap();
        assert_eq!(io::load_from_disk().total_commands, 42);
    }

    /// Work done in a project is counted into that project's own store (a
    /// worktree into its main checkout's); outside any project into none.
    #[test]
    fn each_project_keeps_its_own_counters() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let projects = tempfile::tempdir().unwrap();
        let base = crate::core::pathutil::safe_canonicalize_or_self(projects.path());
        let alpha = base.join("alpha");
        let beta = base.join("beta");
        let worktree = base.join("alpha-wt");
        std::fs::create_dir_all(alpha.join(".git/worktrees/wt")).unwrap();
        std::fs::create_dir_all(beta.join(".git")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", alpha.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        for (folder, calls) in [(&alpha, 2), (&worktree, 1), (&beta, 1)] {
            crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
                std::cell::RefCell::new(Some(folder.clone())),
                || {
                    for _ in 0..calls {
                        record_for_project(|s| apply_record(s, "ctx_read", 100, 40, 0));
                    }
                },
            );
        }

        let scope =
            crate::core::savings_ledger::ProjectScope::from_sessions(&alpha, std::iter::empty());
        let mine = load_for_project(&scope);
        assert_eq!(
            mine.total_commands, 3,
            "the worktree counts for its project"
        );
        assert_eq!(mine.commands["ctx_read"].input_tokens, 300);
        let all = load_all_projects();
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.iter().map(|(_, s)| s.total_commands).sum::<u64>(),
            4,
            "the projects add up"
        );
    }

    #[test]
    fn aggregate_for_display_is_noop_without_siblings() {
        // The common case: only the primary dir has stats. Aggregation must
        // return the primary untouched so non-split users see no change (#500).
        let primary = make_store(7, 1000, 250);
        let agg = aggregate_for_display(primary.clone(), &[]);
        assert_eq!(agg.total_commands, 7);
        assert_eq!(agg.total_input_tokens, 1000);
        assert_eq!(agg.total_output_tokens, 250);
    }

    #[test]
    fn aggregate_for_display_sums_split_dirs() {
        // A data-dir split (#408/#414/#500): the CLI's primary dir is empty but
        // the MCP server wrote its savings into a sibling tree. The displayed
        // total must reflect both so `gain` no longer reports a false `0`.
        let primary = make_store(0, 0, 0);
        let mcp_dir = make_store(12, 8000, 1200);
        let legacy_dir = make_store(3, 500, 100);

        let agg = aggregate_for_display(primary, &[mcp_dir, legacy_dir]);

        assert_eq!(agg.total_commands, 15, "12 (mcp) + 3 (legacy)");
        assert_eq!(agg.total_input_tokens, 8500);
        assert_eq!(agg.total_output_tokens, 1300);
        let saved = agg
            .total_input_tokens
            .saturating_sub(agg.total_output_tokens);
        assert_eq!(saved, 7200, "savings surface despite an empty primary dir");
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
        current
            .command_classes
            .insert("ctx_read".into(), TrafficClass::Compressible);

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
        assert_eq!(
            merged.command_classes["ctx_read"],
            TrafficClass::Compressible
        );
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
    fn apply_deltas_merges_stream_counters_without_replaying_baseline() {
        let mut baseline = StatsStore {
            first_inject_tokens_saved: 1_000,
            reread_tokens_saved: 2_000,
            active_tool_result_tokens_saved: 500,
            last_tool_result_turn: 8,
            stream_tracked_results: 2,
            ..StatsStore::default()
        };
        let mut current = baseline.clone();
        current.first_inject_tokens_saved = 1_400;
        current.reread_tokens_saved = 2_900;
        current.active_tool_result_tokens_saved = 700;
        current.last_tool_result_turn = 10;
        current.stream_tracked_results = 3;
        let disk = StatsStore {
            first_inject_tokens_saved: 5_000,
            reread_tokens_saved: 7_000,
            active_tool_result_tokens_saved: 1_000,
            last_tool_result_turn: 9,
            stream_tracked_results: 10,
            ..StatsStore::default()
        };

        let merged = io::apply_deltas(&disk, &current, &baseline);
        assert_eq!(merged.first_inject_tokens_saved, 5_400);
        assert_eq!(merged.reread_tokens_saved, 7_900);
        assert_eq!(merged.active_tool_result_tokens_saved, 1_200);
        assert_eq!(merged.last_tool_result_turn, 10);
        assert_eq!(merged.stream_tracked_results, 11);

        baseline.first_inject_tokens_saved = current.first_inject_tokens_saved;
        let no_replay = io::apply_deltas(&merged, &current, &baseline);
        assert_eq!(no_replay.first_inject_tokens_saved, 5_400);
    }

    #[test]
    fn merge_and_save_keeps_delta_when_lock_is_busy() {
        let dir = crate::core::data_dir::isolated_data_dir();
        let baseline = make_store(10, 200, 100);
        let current = make_store(11, 300, 120);
        let lock_path = dir.path().join(".stats.lock");
        std::fs::write(&lock_path, "busy").unwrap();

        assert!(io::merge_and_save(&current, &baseline).is_none());

        std::fs::remove_file(lock_path).unwrap();
        let merged = io::merge_and_save(&current, &baseline).unwrap();
        assert_eq!(merged.total_commands, 1);
        assert_eq!(merged.total_input_tokens, 100);
        assert_eq!(merged.total_output_tokens, 20);
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
            compressible_input_tokens: 0,
            compressible_output_tokens: 0,
        }];
        let mut merged_daily = vec![DayStats {
            date: "2026-04-18".to_string(),
            commands: 20,
            input_tokens: 500,
            output_tokens: 490,
            version: String::new(),
            compressible_input_tokens: 0,
            compressible_output_tokens: 0,
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
}
