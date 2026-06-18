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
            // Output-echo summary (#501): how much of recent agent replies
            // re-quoted content that was already in context.
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
