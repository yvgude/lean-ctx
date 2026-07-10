pub(super) fn handle(
    path: &str,
    _query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/stats" => {
            let store = crate::core::stats::load();
            let mut value = serde_json::to_value(&store).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = value.as_object_mut() {
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
            // Measured spend: real model + billed tokens the proxy read from
            // provider responses (cross-process, from proxy_usage.json).
            let per_model = crate::proxy::usage_meter::persisted_snapshot();
            let total_usd: f64 = per_model.iter().map(|m| m.cost_usd).sum();
            // Blended rate (the `fallback-blended` tier) so the dashboard's
            // *estimated* cost model reads its price from the server, not a
            // hardcoded JS constant.
            let blended = crate::core::gain::model_pricing::ModelPricing::load().quote(None);
            let payload = serde_json::json!({
                "source": "measured",
                "available": !per_model.is_empty(),
                "total_usd": total_usd,
                "per_model": per_model,
                "pricing": {
                    "input_per_m": blended.cost.input_per_m,
                    "output_per_m": blended.cost.output_per_m,
                },
                "note": "Real provider bill for proxy-routed clients (Claude Code, Codex, Pi, Gemini CLI, OpenCode). MCP-only IDEs are priced as estimated.",
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        _ => None,
    }
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
        "cli_full" | "cli_ls" | "cli_read_dedup" => "redirect",
        "cli_shell" | "cli_grep" | "cli_map" | "cli_signatures" => "rewrite",
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
