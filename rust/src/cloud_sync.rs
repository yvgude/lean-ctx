use crate::core::config::Config;

/// Outcome of one background Personal-Cloud auto-push (GL #384).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSyncOutcome {
    /// At least one surface pushed (or there was nothing to push).
    Synced,
    /// The server gated sync behind Pro (HTTP 402) — stop for today.
    Gated,
    /// Every push failed without a 402 (offline / server down) — try again
    /// at the next opportunity, do not consume today's slot.
    NetworkFailure,
}

/// Whether the auto-sync should run now: opt-in flag, logged in, and not
/// already synced today (the debounce). Pure for unit testing.
#[must_use]
pub fn should_auto_sync(
    auto_sync: bool,
    logged_in: bool,
    last_auto_sync: Option<&str>,
    today: &str,
) -> bool {
    auto_sync && logged_in && last_auto_sync != Some(today)
}

/// Whether the background index push should run for this project (GL #392):
/// separate opt-in, logged in, a local index actually exists, and this
/// project hasn't pushed today. Pure for unit testing.
#[must_use]
pub fn should_auto_push_index(
    auto_index: bool,
    logged_in: bool,
    local_index_exists: bool,
    last_push_for_project: Option<&str>,
    today: &str,
) -> bool {
    auto_index && logged_in && local_index_exists && last_push_for_project != Some(today)
}

/// Classify per-surface push results into one [`AutoSyncOutcome`]. A 402
/// anywhere wins (the account is gated); otherwise total failure means the
/// network is down; anything else counts as synced.
#[must_use]
pub fn classify_outcomes(results: &[Result<(), String>]) -> AutoSyncOutcome {
    if results
        .iter()
        .any(|r| r.as_ref().is_err_and(|e| e.contains("402")))
    {
        return AutoSyncOutcome::Gated;
    }
    if !results.is_empty() && results.iter().all(Result::is_err) {
        return AutoSyncOutcome::NetworkFailure;
    }
    AutoSyncOutcome::Synced
}

pub fn cloud_background_tasks() {
    // Persist path: read global-only so the daily background save never leaks a
    // project-local override into the global config (#443).
    let mut config = Config::load_global();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let already_contributed = config
        .cloud
        .last_contribute
        .as_deref()
        .is_some_and(|d| d == today);
    let already_synced = config
        .cloud
        .last_sync
        .as_deref()
        .is_some_and(|d| d == today);
    let already_gain_synced = config
        .cloud
        .last_gain_sync
        .as_deref()
        .is_some_and(|d| d == today);
    let already_pulled = config
        .cloud
        .last_model_pull
        .as_deref()
        .is_some_and(|d| d == today);

    if config.cloud.contribute_enabled && !already_contributed {
        let entries = collect_contribute_entries();
        if !entries.is_empty() && crate::cloud_client::contribute(&entries).is_ok() {
            config.cloud.last_contribute = Some(today.clone());
        }
    }

    if crate::cloud_client::is_logged_in() {
        if !already_synced {
            let store = crate::core::stats::load();
            let entries = build_sync_entries(&store);
            if !entries.is_empty() && crate::cloud_client::sync_stats(&entries).is_ok() {
                config.cloud.last_sync = Some(today.clone());
            }
        }

        if !already_gain_synced {
            let engine = crate::core::gain::GainEngine::load();
            let summary = engine.summary(None);
            let trend = match summary.score.trend {
                crate::core::gain::gain_score::Trend::Rising => "rising",
                crate::core::gain::gain_score::Trend::Stable => "stable",
                crate::core::gain::gain_score::Trend::Declining => "declining",
            };
            let entry = serde_json::json!({
                "recorded_at": format!("{today}T00:00:00Z"),
                "total": f64::from(summary.score.total),
                "compression": f64::from(summary.score.compression),
                "cost_efficiency": f64::from(summary.score.cost_efficiency),
                "quality": f64::from(summary.score.quality),
                "consistency": f64::from(summary.score.consistency),
                "trend": trend,
                "avoided_usd": summary.avoided_usd,
                "tool_spend_usd": summary.tool_spend_usd,
                "model_key": summary.model.model_key,
            });
            if crate::cloud_client::push_gain(&[entry]).is_ok() {
                config.cloud.last_gain_sync = Some(today.clone());
            }
        }

        if !already_pulled && let Ok(data) = crate::cloud_client::pull_cloud_models() {
            let _ = crate::cloud_client::save_cloud_models(&data);
            config.cloud.last_model_pull = Some(today.clone());
        }

        // Opt-in Personal-Cloud auto-push (GL #384): silent, once per day,
        // offline-tolerant. A network failure leaves the slot open so the
        // next background cycle retries; a Pro gate consumes it (one quiet
        // attempt per day on a Free account, never error spam).
        if should_auto_sync(
            config.cloud.auto_sync,
            true,
            config.cloud.last_auto_sync.as_deref(),
            &today,
        ) && auto_sync_personal_cloud() != AutoSyncOutcome::NetworkFailure
        {
            config.cloud.last_auto_sync = Some(today.clone());
        }

        // Opt-in hosted-index auto-push (GL #392): once per project per day,
        // only when a local index exists. Quota/Pro rejections consume the
        // slot (one quiet attempt per day); network failures leave it open.
        if let Ok(root) = std::env::current_dir() {
            let project_hash = crate::core::index_namespace::namespace_hash(&root);
            if should_auto_push_index(
                config.cloud.auto_index,
                true,
                crate::core::index_bundle::local_index_present(&root),
                config
                    .cloud
                    .last_index_push
                    .get(&project_hash)
                    .map(String::as_str),
                &today,
            ) {
                match crate::cloud_client::push_index_bundle(&root) {
                    Ok((hash, bytes)) => {
                        tracing::debug!(project = %hash, bytes, "auto-index: pushed");
                        config
                            .cloud
                            .last_index_push
                            .insert(project_hash, today.clone());
                    }
                    Err(e) if e.contains("Pro") || e.contains("Quota") => {
                        tracing::debug!(error = %e, "auto-index: gated, retry tomorrow");
                        config
                            .cloud
                            .last_index_push
                            .insert(project_hash, today.clone());
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "auto-index: push failed, slot stays open");
                    }
                }
            }
        }
    }

    if let Err(e) = config.save() {
        tracing::warn!("could not persist cloud background state: {e}");
    }
}

/// Push every Personal-Cloud surface silently (background variant of
/// `lean-ctx sync`'s interactive flow — tracing instead of stdout).
fn auto_sync_personal_cloud() -> AutoSyncOutcome {
    let store = crate::core::stats::load();
    let mut results: Vec<Result<(), String>> = Vec::new();

    let mut push = |label: &str, result: Result<String, String>| match result {
        Ok(_) => {
            tracing::debug!(surface = label, "auto-sync: pushed");
            results.push(Ok(()));
        }
        Err(e) => {
            tracing::debug!(surface = label, error = %e, "auto-sync: push failed");
            results.push(Err(e));
        }
    };

    let commands = collect_command_entries(&store);
    if !commands.is_empty() {
        push("commands", crate::cloud_client::push_commands(&commands));
    }
    let cep = collect_cep_entries(&store);
    if !cep.is_empty() {
        push("cep", crate::cloud_client::push_cep(&cep));
    }
    let knowledge = collect_knowledge_entries();
    if !knowledge.is_empty() {
        push("knowledge", crate::cloud_client::push_knowledge(&knowledge));
    }
    let gotchas = collect_gotcha_entries();
    if !gotchas.is_empty() {
        push("gotchas", crate::cloud_client::push_gotchas(&gotchas));
    }
    let buddy = crate::core::buddy::BuddyState::compute();
    if let Ok(buddy_data) = serde_json::to_value(&buddy) {
        push("buddy", crate::cloud_client::push_buddy(&buddy_data));
    }
    let feedback = collect_feedback_entries();
    if !feedback.is_empty() {
        push("feedback", crate::cloud_client::push_feedback(&feedback));
    }

    let outcome = classify_outcomes(&results);
    tracing::info!(
        ?outcome,
        surfaces = results.len(),
        "personal-cloud auto-sync done"
    );
    outcome
}

#[must_use]
pub fn build_sync_entries(store: &crate::core::stats::StatsStore) -> Vec<serde_json::Value> {
    let mut entries = Vec::new();
    let cep = &store.cep;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let mut cep_cache_by_day: std::collections::HashMap<String, (u64, u64)> =
        std::collections::HashMap::new();
    for s in &cep.scores {
        if let Some(date) = s.timestamp.get(..10) {
            let entry = cep_cache_by_day.entry(date.to_string()).or_default();
            let calls = s.tool_calls.max(1);
            let hits = (calls as f64 * f64::from(s.cache_hit_rate) / 100.0).round() as u64;
            entry.0 += calls;
            entry.1 += hits;
        }
    }

    let mut mcp_saved_total = 0u64;
    for (cmd, s) in &store.commands {
        if cmd.starts_with("ctx_") {
            mcp_saved_total += s.input_tokens.saturating_sub(s.output_tokens);
        }
    }
    let global_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens)
        .max(1);
    let mcp_ratio = mcp_saved_total as f64 / global_saved as f64;

    for day in &store.daily {
        let tokens_original = day.input_tokens;
        let tokens_compressed = day.output_tokens;
        let tokens_saved = tokens_original.saturating_sub(tokens_compressed);
        let (day_calls, day_hits) = cep_cache_by_day.get(&day.date).copied().unwrap_or((0, 0));
        let day_mcp_saved = (tokens_saved as f64 * mcp_ratio).round() as u64;
        let day_hook_saved = tokens_saved.saturating_sub(day_mcp_saved);
        entries.push(serde_json::json!({
            "date": day.date,
            "tokens_original": tokens_original,
            "tokens_compressed": tokens_compressed,
            "tokens_saved": tokens_saved,
            "mcp_tokens_saved": day_mcp_saved,
            "hook_tokens_saved": day_hook_saved,
            "tool_calls": day.commands,
            "cache_hits": day_hits,
            "cache_misses": day_calls.saturating_sub(day_hits),
        }));
    }

    let has_today = entries.iter().any(|e| e["date"].as_str() == Some(&today));
    if !has_today && (cep.total_tokens_original > 0 || store.total_commands > 0) {
        let today_saved = cep
            .total_tokens_original
            .saturating_sub(cep.total_tokens_compressed);
        let today_mcp = (today_saved as f64 * mcp_ratio).round() as u64;
        entries.push(serde_json::json!({
            "date": today,
            "tokens_original": cep.total_tokens_original,
            "tokens_compressed": cep.total_tokens_compressed,
            "tokens_saved": today_saved,
            "mcp_tokens_saved": today_mcp,
            "hook_tokens_saved": today_saved.saturating_sub(today_mcp),
            "tool_calls": store.total_commands,
            "cache_hits": cep.total_cache_hits,
            "cache_misses": cep.total_cache_reads.saturating_sub(cep.total_cache_hits),
        }));
    }

    entries
}

// ── Personal-Cloud surface collectors ────────────────────────────────────────
// Shared by the interactive `lean-ctx sync` flow and the background auto-sync
// (GL #384): pure local reads, no network, no stdout.

#[must_use]
pub fn collect_knowledge_entries() -> Vec<serde_json::Value> {
    let Ok(data_dir) = crate::core::paths::data_dir() else {
        return Vec::new();
    };
    let knowledge_dir = data_dir.join("knowledge");
    if !knowledge_dir.is_dir() {
        return Vec::new();
    }

    let mut entries = Vec::new();

    for project_entry in std::fs::read_dir(&knowledge_dir).into_iter().flatten() {
        let Ok(project_entry) = project_entry else {
            continue;
        };
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }

        for file_entry in std::fs::read_dir(&project_path).into_iter().flatten() {
            let Ok(file_entry) = file_entry else { continue };
            let file_path = file_entry.path();
            if file_path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(data) = std::fs::read_to_string(&file_path) else {
                continue;
            };
            let parsed: serde_json::Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Some(facts) = parsed["facts"].as_array() {
                for fact in facts {
                    let cat = fact["category"].as_str().unwrap_or("general");
                    let key = fact["key"].as_str().unwrap_or("");
                    let val = fact["value"]
                        .as_str()
                        .or_else(|| fact["description"].as_str())
                        .unwrap_or("");
                    if !key.is_empty() {
                        entries.push(serde_json::json!({
                            "category": cat,
                            "key": key,
                            "value": val,
                        }));
                    }
                }
            }

            if let Some(gotchas) = parsed["gotchas"].as_array() {
                for g in gotchas {
                    let pattern = g["pattern"].as_str().unwrap_or("");
                    let fix = g["fix"].as_str().unwrap_or("");
                    if !pattern.is_empty() {
                        entries.push(serde_json::json!({
                            "category": "gotcha",
                            "key": pattern,
                            "value": fix,
                        }));
                    }
                }
            }
        }
    }

    entries
}

#[must_use]
pub fn collect_command_entries(store: &crate::core::stats::StatsStore) -> Vec<serde_json::Value> {
    store
        .commands
        .iter()
        .map(|(name, stats)| {
            let tokens_saved = stats.input_tokens.saturating_sub(stats.output_tokens);
            serde_json::json!({
                "command": name,
                "source": if name.starts_with("ctx_") { "mcp" } else { "hook" },
                "count": stats.count,
                "input_tokens": stats.input_tokens,
                "output_tokens": stats.output_tokens,
                "tokens_saved": tokens_saved,
            })
        })
        .collect()
}

fn complexity_to_float(s: &str) -> f64 {
    match s.to_lowercase().as_str() {
        "trivial" => 0.1,
        "simple" => 0.3,
        "moderate" => 0.5,
        "complex" => 0.7,
        "architectural" => 0.9,
        other => other.parse::<f64>().unwrap_or(0.5),
    }
}

#[must_use]
pub fn collect_cep_entries(store: &crate::core::stats::StatsStore) -> Vec<serde_json::Value> {
    store
        .cep
        .scores
        .iter()
        .map(|s| {
            serde_json::json!({
                "recorded_at": s.timestamp,
                "score": f64::from(s.score) / 100.0,
                "cache_hit_rate": f64::from(s.cache_hit_rate) / 100.0,
                "mode_diversity": f64::from(s.mode_diversity) / 100.0,
                "compression_rate": f64::from(s.compression_rate) / 100.0,
                "tool_calls": s.tool_calls,
                "tokens_saved": s.tokens_saved,
                "complexity": complexity_to_float(&s.complexity),
            })
        })
        .collect()
}

#[must_use]
pub fn collect_gotcha_entries() -> Vec<serde_json::Value> {
    let mut all_gotchas = crate::core::gotcha_tracker::load_universal_gotchas();

    if let Ok(knowledge_dir) = crate::core::paths::data_dir().map(|d| d.join("knowledge"))
        && let Ok(entries) = std::fs::read_dir(&knowledge_dir)
    {
        for entry in entries.flatten() {
            let gotcha_path = entry.path().join("gotchas.json");
            if gotcha_path.exists()
                && let Ok(content) = std::fs::read_to_string(&gotcha_path)
                && let Ok(store) =
                    serde_json::from_str::<crate::core::gotcha_tracker::GotchaStore>(&content)
            {
                for g in store.gotchas {
                    if !all_gotchas
                        .iter()
                        .any(|existing| existing.trigger == g.trigger)
                    {
                        all_gotchas.push(g);
                    }
                }
            }
        }
    }

    all_gotchas
        .iter()
        .map(|g| {
            serde_json::json!({
                "pattern": g.trigger,
                "fix": g.resolution,
                "severity": format!("{:?}", g.severity).to_lowercase(),
                "category": format!("{:?}", g.category).to_lowercase(),
                "occurrences": g.occurrences,
                "prevented_count": g.prevented_count,
                "confidence": g.confidence,
            })
        })
        .collect()
}

#[must_use]
pub fn collect_feedback_entries() -> Vec<serde_json::Value> {
    let store = crate::core::feedback::FeedbackStore::load();
    store
        .learned_thresholds
        .iter()
        .map(|(lang, thresholds)| {
            serde_json::json!({
                "language": lang,
                "entropy": thresholds.entropy,
                "jaccard": thresholds.jaccard,
                "sample_count": thresholds.sample_count,
                "avg_efficiency": thresholds.avg_efficiency,
            })
        })
        .collect()
}

#[must_use]
pub fn collect_contribute_entries() -> Vec<serde_json::Value> {
    let mut entries = Vec::new();

    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let mode_stats_path = data_dir.join("mode_stats.json");
        if let Ok(data) = std::fs::read_to_string(&mode_stats_path)
            && let Ok(predictor) = serde_json::from_str::<serde_json::Value>(&data)
            && let Some(history) = predictor["history"].as_object()
        {
            for (_key, outcomes) in history {
                if let Some(arr) = outcomes.as_array() {
                    for outcome in arr.iter().rev().take(3) {
                        let ext = outcome["ext"].as_str().unwrap_or("unknown");
                        let mode = outcome["mode"].as_str().unwrap_or("full");
                        let t_in = outcome["tokens_in"].as_u64().unwrap_or(0);
                        let t_out = outcome["tokens_out"].as_u64().unwrap_or(0);
                        let ratio = if t_in > 0 {
                            1.0 - t_out as f64 / t_in as f64
                        } else {
                            0.0
                        };
                        let bucket = match t_in {
                            0..=500 => "0-500",
                            501..=2000 => "500-2k",
                            2001..=10000 => "2k-10k",
                            _ => "10k+",
                        };
                        entries.push(serde_json::json!({
                            "file_ext": format!(".{ext}"),
                            "size_bucket": bucket,
                            "best_mode": mode,
                            "compression_ratio": (ratio * 100.0).round() / 100.0,
                        }));
                        if entries.len() >= 200 {
                            return entries;
                        }
                    }
                }
            }
        }
    }

    if entries.is_empty() {
        let stats_data = crate::core::stats::format_gain_json();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stats_data) {
            let original = parsed["cep"]["total_tokens_original"].as_u64().unwrap_or(0);
            let compressed = parsed["cep"]["total_tokens_compressed"]
                .as_u64()
                .unwrap_or(0);
            let ratio = if original > 0 {
                1.0 - compressed as f64 / original as f64
            } else {
                0.0
            };
            if let Some(modes) = parsed["cep"]["modes"].as_object() {
                let read_modes = [
                    "full",
                    "map",
                    "signatures",
                    "auto",
                    "aggressive",
                    "entropy",
                    "diff",
                    "lines",
                    "task",
                    "reference",
                ];
                for (mode, count) in modes {
                    if !read_modes.contains(&mode.as_str()) || count.as_u64().unwrap_or(0) == 0 {
                        continue;
                    }
                    entries.push(serde_json::json!({
                        "file_ext": "mixed",
                        "size_bucket": "mixed",
                        "best_mode": mode,
                        "compression_ratio": (ratio * 100.0).round() / 100.0,
                    }));
                }
            }
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_sync_requires_flag_login_and_unused_slot() {
        // Disabled flag blocks everything else.
        assert!(!should_auto_sync(false, true, None, "2026-06-10"));
        // Logged out never syncs.
        assert!(!should_auto_sync(true, false, None, "2026-06-10"));
        // Fresh slot + flag + login → go.
        assert!(should_auto_sync(true, true, None, "2026-06-10"));
        // Already synced today → debounced.
        assert!(!should_auto_sync(
            true,
            true,
            Some("2026-06-10"),
            "2026-06-10"
        ));
        // Synced yesterday → today's slot is free.
        assert!(should_auto_sync(
            true,
            true,
            Some("2026-06-09"),
            "2026-06-10"
        ));
    }

    #[test]
    fn auto_index_push_needs_flag_login_index_and_fresh_slot() {
        let t = "2026-06-10";
        // All preconditions met → push.
        assert!(should_auto_push_index(true, true, true, None, t));
        // Separate opt-in: auto_sync users are NOT auto-enrolled.
        assert!(!should_auto_push_index(false, true, true, None, t));
        // Logged out / no local index → silently skip, no error path.
        assert!(!should_auto_push_index(true, false, true, None, t));
        assert!(!should_auto_push_index(true, true, false, None, t));
        // Per-project debounce: today consumed, yesterday frees the slot.
        assert!(!should_auto_push_index(true, true, true, Some(t), t));
        assert!(should_auto_push_index(
            true,
            true,
            true,
            Some("2026-06-09"),
            t
        ));
    }

    #[test]
    fn outcome_classification_is_gate_then_network_then_synced() {
        // Nothing to push counts as synced (slot consumed, no retry storm).
        assert_eq!(classify_outcomes(&[]), AutoSyncOutcome::Synced);
        // Any 402 means the account is gated, even with other failures.
        assert_eq!(
            classify_outcomes(&[
                Err("HTTP 402: upgrade required".into()),
                Err("connection refused".into()),
            ]),
            AutoSyncOutcome::Gated
        );
        // All failed without a 402 → offline, keep the slot open.
        assert_eq!(
            classify_outcomes(&[Err("connection refused".into()), Err("timeout".into()),]),
            AutoSyncOutcome::NetworkFailure
        );
        // Partial success is success.
        assert_eq!(
            classify_outcomes(&[Ok(()), Err("timeout".into())]),
            AutoSyncOutcome::Synced
        );
    }
}
