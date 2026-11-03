// SPDX-License-Identifier: Apache-2.0
use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use lean_ctx::tool_defs::tool_def;

pub struct CtxMetricsTool;

impl McpTool for CtxMetricsTool {
    fn name(&self) -> &'static str {
        "ctx_metrics"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_metrics",
            "Session token statistics — cache hit rates, per-tool savings, pipeline metrics,\n\
 and signature backend ratios.\n\
 ANTI-PATTERN: not for real-time monitoring — snapshot of current session.\n\
 Complements ctx_radar for budget analysis.",
            json!({
                "type": "object",
                "properties": {}
            }),
        )
    }

    fn handle(
        &self,
        _args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(cache_guard) = crate::server::bounded_lock::read(cache, "ctx_metrics:cache")
        else {
            return Ok(ToolOutput::simple(
                "[metrics unavailable — cache busy, retry]".to_string(),
            ));
        };
        let calls = ctx
            .tool_calls
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("tool_calls not available", None))?;
        let Some(calls_guard) = crate::server::bounded_lock::read(calls, "ctx_metrics:calls")
        else {
            return Ok(ToolOutput::simple(
                "[metrics unavailable — calls lock busy, retry]".to_string(),
            ));
        };
        let mut result =
            crate::tools::ctx_metrics::handle(&cache_guard, &calls_guard, ctx.crp_mode);
        drop(cache_guard);
        drop(calls_guard);

        if let Some(ref ps) = ctx.pipeline_stats {
            let Some(stats) = crate::server::bounded_lock::read(ps, "ctx_metrics:pipeline") else {
                return Ok(ToolOutput::simple(result));
            };
            if stats.runs > 0 {
                result.push_str("\n\n--- PIPELINE METRICS ---\n");
                result.push_str(&stats.format_summary());
            }
        }

        let (ts_hits, regex_hits) = lean_ctx::core::signatures::signature_backend_stats();
        if ts_hits + regex_hits > 0 {
            result.push_str("\n--- SIGNATURE BACKEND ---\n");
            result.push_str(&format!(
                "tree-sitter: {} | regex fallback: {} | ratio: {:.0}%\n",
                ts_hits,
                regex_hits,
                if ts_hits + regex_hits > 0 {
                    ts_hits as f64 / (ts_hits + regex_hits) as f64 * 100.0
                } else {
                    0.0
                }
            ));
            // Persistent per-extension split (GH #690 Phase 2 telemetry) —
            // top rows only; full history lives in grammar_usage.json.
            let ranked = lean_ctx::core::grammar_usage::live_ranked();
            if !ranked.is_empty() {
                let rows: Vec<String> = ranked
                    .iter()
                    .take(8)
                    .map(|(ext, u)| {
                        format!(".{ext}: ts={} rx={}", u.tree_sitter_hits, u.regex_hits)
                    })
                    .collect();
                result.push_str(&format!("by extension (all-time): {}\n", rows.join(" | ")));
            }
        }

        Ok(ToolOutput::simple(result))
    }
}
