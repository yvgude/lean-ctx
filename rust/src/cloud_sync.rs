use crate::core::config::Config;

pub fn cloud_background_tasks() {
    let mut config = Config::load();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let already_contributed = config
        .cloud
        .last_contribute
        .as_deref()
        .map(|d| d == today)
        .unwrap_or(false);
    let already_synced = config
        .cloud
        .last_sync
        .as_deref()
        .map(|d| d == today)
        .unwrap_or(false);
    let already_gain_synced = config
        .cloud
        .last_gain_sync
        .as_deref()
        .map(|d| d == today)
        .unwrap_or(false);
    let already_pulled = config
        .cloud
        .last_model_pull
        .as_deref()
        .map(|d| d == today)
        .unwrap_or(false);

    if config.cloud.contribute_enabled && !already_contributed {
        let entries = collect_contribute_entries();
        if !entries.is_empty() && crate::cloud_client::contribute(&entries).is_ok() {
            config.cloud.last_contribute = Some(today.clone());
        }
    }

    if crate::cloud_client::is_logged_in() {
        if !already_synced {
            let store = crate::core::stats::load();
            let cep = &store.cep;
            let entry = serde_json::json!({
                "date": &today,
                "tokens_original": cep.total_tokens_original,
                "tokens_compressed": cep.total_tokens_compressed,
                "tokens_saved": cep.total_tokens_original.saturating_sub(cep.total_tokens_compressed),
                "tool_calls": store.total_commands,
                "cache_hits": cep.total_cache_hits,
                "cache_misses": cep.total_cache_reads.saturating_sub(cep.total_cache_hits),
            });
            if crate::cloud_client::sync_stats(&[entry]).is_ok() {
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
                "total": summary.score.total as f64,
                "compression": summary.score.compression as f64,
                "cost_efficiency": summary.score.cost_efficiency as f64,
                "quality": summary.score.quality as f64,
                "consistency": summary.score.consistency as f64,
                "trend": trend,
                "avoided_usd": summary.avoided_usd,
                "tool_spend_usd": summary.tool_spend_usd,
                "model_key": summary.model.model_key,
            });
            if crate::cloud_client::push_gain(&[entry]).is_ok() {
                config.cloud.last_gain_sync = Some(today.clone());
            }
        }

        if !already_pulled {
            if let Ok(data) = crate::cloud_client::pull_cloud_models() {
                let _ = crate::cloud_client::save_cloud_models(&data);
                config.cloud.last_model_pull = Some(today.clone());
            }
        }
    }

    let _ = config.save();
}

pub fn collect_contribute_entries() -> Vec<serde_json::Value> {
    let mut entries = Vec::new();

    if let Some(home) = dirs::home_dir() {
        let mode_stats_path = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap_or_else(|_| home.join(".lean-ctx"))
            .join("mode_stats.json");
        if let Ok(data) = std::fs::read_to_string(&mode_stats_path) {
            if let Ok(predictor) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(history) = predictor["history"].as_object() {
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
