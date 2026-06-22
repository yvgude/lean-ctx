use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxRadarTool;

impl McpTool for CtxRadarTool {
    fn name(&self) -> &'static str {
        "ctx_radar"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_radar",
            "Context budget breakdown — system prompt, messages, tools, reads, shell.\n\
             WORKFLOW: call when context window tight to find biggest consumers.\n\
             ANTIPATTERN: not for per-call timing — use ctx_metrics instead.\n\
             format=display (human-readable) or json (structured). Complements ctx_metrics\n\
             for comprehensive budget analysis. Saves tokens vs manual budget estimation.",
            json!({
                "type": "object",
                "properties": {
                    "format": {
                        "type": "string",
                        "description": "display|json",
                        "enum": ["display", "json"],
                        "default": "display"
                    }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let format = args
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("display");

        let data_dir = crate::core::data_dir::lean_ctx_data_dir().unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
                .join(".lean-ctx")
        });

        let client_name = ctx
            .client_name
            .as_ref()
            .and_then(|cn| tokio::task::block_in_place(|| cn.blocking_read().clone()).into())
            .unwrap_or_else(|| "cursor".to_string());
        let window_size = crate::core::context_radar::default_window_for_client(&client_name);

        let radar = crate::core::context_radar::ContextRadar::load(&data_dir, window_size);

        let output = match format {
            "json" => {
                let breakdown = radar.budget_breakdown();
                serde_json::to_string_pretty(&breakdown).unwrap_or_default()
            }
            _ => radar.format_display(),
        };

        Ok(ToolOutput::simple(output))
    }
}
