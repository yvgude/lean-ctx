pub(super) fn handle(
    path: &str,
    _query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/stats" => {
            // A named project reads its own counters (kept per project since
            // they were split); the device view keeps the device total.
            let scope = super::helpers::request_ledger_scope();
            let store = match &scope {
                Some(scope) => crate::core::stats::load_for_project(scope),
                None => crate::core::stats::load_for_display(),
            };
            let mut value = serde_json::to_value(&store).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = value.as_object_mut() {
                if scope.is_some() {
                    // Live process figures that no project owns.
                    obj.insert(
                        "device_wide".to_string(),
                        serde_json::json!(["output_echo", "edit_efficiency", "cache_runtime"]),
                    );
                }
                let echo = crate::core::output_echo::load_stats();
                obj.insert(
                    "output_echo".to_string(),
                    serde_json::json!({
                        "avg_ratio": echo.avg_ratio(50),
                        "window": echo.reports.len(),
                        "total_analyzed": echo.total_analyzed,
                    }),
                );
                obj.insert(
                    "edit_efficiency".to_string(),
                    crate::core::edit_metering::metrics_snapshot(),
                );
                obj.insert("channel_breakdown".to_string(), channel_breakdown(&store));
                if let Some(cache_runtime) = load_cache_runtime() {
                    obj.insert("cache_runtime".to_string(), cache_runtime);
                }
            }
            let json = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/gain" => {
            let env_model = std::env::var("LEAN_CTX_MODEL")
                .or_else(|_| std::env::var("LCTX_MODEL"))
                .ok();
            let engine = crate::core::gain::GainEngine::load();
            let payload = serde_json::json!({
                "summary": engine.summary(env_model.as_deref()),
                "tasks": engine.task_breakdown(),
                "heatmap": engine.heatmap_gains(20),
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/pulse" => {
            let stats_path = crate::core::data_dir::lean_ctx_data_dir()
                .map(|d| d.join("stats.json"))
                .unwrap_or_default();
            let meta = std::fs::metadata(&stats_path).ok();
            let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
            let mtime = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            use md5::Digest;
            let hash = crate::core::agent_identity::hex_encode(&md5::Md5::digest(
                format!("{size}-{mtime}").as_bytes(),
            ));
            let json = format!(r#"{{"hash":"{hash}","ts":{mtime}}}"#);
            Some(("200 OK", "application/json", json))
        }
        "/api/pipeline-stats" => {
            let stats = crate::core::pipeline::PipelineStats::load();
            let json = serde_json::to_string(&stats).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/spend" => {
            // Provider-reported tokens per model (cross-process, from
            // proxy_usage.json), priced at list price unless a response
            // carried the provider's own charge (`measured_cost_usd`). A named
            // project reads its share: the turns whose request named a folder
            // of it. The device view is the device total, split per project.
            let scope = super::helpers::request_ledger_scope();
            let per_model = match &scope {
                Some(scope) => {
                    let keys = scope.repo_hashes();
                    crate::proxy::usage_meter::persisted_snapshot_where(|key| {
                        keys.iter().any(|k| k == key)
                    })
                }
                None => crate::proxy::usage_meter::persisted_snapshot(),
            };
            let total_usd: f64 = per_model.iter().map(|m| m.cost_usd).sum();
            let charged_usd: f64 = per_model.iter().map(|m| m.measured_cost_usd).sum();
            // Token counts are measured; money is list price unless every
            // dollar came from a provider-reported charge.
            let source = if charged_usd > 0.0 && (total_usd - charged_usd).abs() < 0.005 {
                "provider_charge"
            } else {
                "list_price"
            };
            // Blended rate (the `fallback-blended` tier) so the dashboard's
            // *estimated* cost model reads its price from the server, not a
            // hardcoded JS constant.
            let blended = crate::core::gain::model_pricing::ModelPricing::load().quote(None);
            let mut payload = serde_json::json!({
                "source": source,
                "charged_usd": charged_usd,
                "available": !per_model.is_empty(),
                "total_usd": total_usd,
                "per_model": per_model,
                "pricing": {
                    "input_per_m": blended.cost.input_per_m,
                    "output_per_m": blended.cost.output_per_m,
                },
                "note": "Provider-reported tokens of proxy-routed clients at public API list prices, except where a response carried the provider's own charge. Not an invoice; subscription plans are not billed per token.",
            });
            if scope.is_none() {
                payload["by_project"] = spend_by_project(total_usd);
            }
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        _ => None,
    }
}

/// The device's spend split per project, named like the project switch, and
/// the rest no project can be named for (turns before projects were kept, or
/// whose request named no folder).
fn spend_by_project(total_usd: f64) -> serde_json::Value {
    let names = super::helpers::project_names();
    let mut by_name: std::collections::BTreeMap<String, (f64, u64)> = Default::default();
    let mut assigned = 0.0;
    for (key, models) in crate::proxy::usage_meter::persisted_projects() {
        let cost: f64 = models.iter().map(|m| m.cost_usd).sum();
        let requests: u64 = models.iter().map(|m| m.requests).sum();
        assigned += cost;
        let name = names
            .get(&key)
            .cloned()
            .unwrap_or_else(|| "Other folder".to_string());
        let row = by_name.entry(name).or_default();
        row.0 += cost;
        row.1 += requests;
    }
    let mut rows: Vec<serde_json::Value> = by_name
        .into_iter()
        .map(|(project, (cost, requests))| {
            serde_json::json!({"project": project, "cost_usd": cost, "requests": requests})
        })
        .collect();
    rows.sort_by(|a, b| {
        b["cost_usd"]
            .as_f64()
            .partial_cmp(&a["cost_usd"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    serde_json::json!({
        "projects": rows,
        "unassigned_usd": (total_usd - assigned).max(0.0),
    })
}

/// Current MCP-process cache telemetry. Historical cache totals remain in
/// `StatsStore::cep`; this snapshot makes the hot cache visible to the live
/// cockpit without teaching the browser where runtime state files live.

fn load_cache_runtime() -> Option<serde_json::Value> {
    let mcp_path = crate::core::paths::state_dir().ok()?.join("mcp-live.json");
    let live: serde_json::Value = std::fs::read_to_string(&mcp_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let u_mcp = |key: &str| -> u64 {
        live.get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let f_mcp = |key: &str| -> f64 {
        live.get(key)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0)
    };

    // Merge hook-side stats from stats.json so shadow-mode reads are visible.
    let hook_store = crate::core::stats::load_for_display();
    let hook_totals = hook_store.compression_totals();
    let hook_reads = hook_store.total_commands;

    let total_reads = u_mcp("total_reads") + hook_reads;
    let tokens_saved = u_mcp("tokens_saved")
        + hook_totals
            .input_tokens
            .saturating_sub(hook_totals.output_tokens);
    let tokens_original = u_mcp("tokens_original") + hook_totals.input_tokens;
    let cache_hits = u_mcp("cache_hits");

    Some(serde_json::json!({
        "cache_hits": cache_hits,
        "total_reads": total_reads,
        "tokens_saved": tokens_saved,
        "tokens_original": tokens_original,
        "read_reuse_rate": f_mcp("read_reuse_rate"),
        "disk_reuse_rate": f_mcp("disk_reuse_rate"),
        "provider_reuse_rate": f_mcp("provider_reuse_rate"),
        "read_reuse": live.get("read_reuse").cloned().unwrap_or_else(|| serde_json::json!({})),
        "disk_reuse": live.get("disk_reuse").cloned().unwrap_or_else(|| serde_json::json!({})),
        "provider_reuse": live.get("provider_reuse").cloned().unwrap_or_else(|| serde_json::json!({})),
        "reuse_outcomes": live.get("reuse_outcomes").cloned().unwrap_or_else(|| serde_json::json!({})),
        "dedup_hits": u_mcp("dedup_hits"),
        "compressed_cache_hits": u_mcp("compressed_cache_hits"),
        "full_delivery_degraded": u_mcp("full_delivery_degraded"),
    }))
}

/// Classify every recorded command into its delivery channel:
///
/// - **redirect**: `cli_full`, `cli_ls`, `cli_grep` — native tool calls
///   intercepted by the PreToolUse hook and redirected through lean-ctx
/// - **rewrite**: `cli_shell`, `cli_map`, `cli_signatures` — shell commands
///   rewritten to use lean-ctx (e.g. `grep` → `lean-ctx grep`)
/// - **mcp**: `ctx_*` — direct MCP tool calls to the lean-ctx server
fn channel_breakdown(store: &crate::core::stats::StatsStore) -> serde_json::Value {
    let (mut rd, mut rw, mut mcp) = (
        ChannelAgg::default(),
        ChannelAgg::default(),
        ChannelAgg::default(),
    );
    for (cmd, s) in &store.commands {
        let ch = classify_channel(cmd);
        let agg = match ch {
            "redirect" => &mut rd,
            "rewrite" => &mut rw,
            _ => &mut mcp,
        };
        agg.calls += s.count;
        agg.input += s.input_tokens;
        agg.output += s.output_tokens;
    }
    serde_json::json!({
        "redirect": { "calls": rd.calls, "input_tokens": rd.input, "output_tokens": rd.output, "saved": rd.input.saturating_sub(rd.output) },
        "rewrite":  { "calls": rw.calls, "input_tokens": rw.input, "output_tokens": rw.output, "saved": rw.input.saturating_sub(rw.output) },
        "mcp":      { "calls": mcp.calls, "input_tokens": mcp.input, "output_tokens": mcp.output, "saved": mcp.input.saturating_sub(mcp.output) },
    })
}

fn classify_channel(cmd: &str) -> &'static str {
    match cmd {
        c if c.starts_with("cli_") => "redirect",
        c if c.starts_with("ctx_") => "mcp",
        _ => "mcp",
    }
}

#[derive(Default)]
struct ChannelAgg {
    calls: u64,
    input: u64,
    output: u64,
}
