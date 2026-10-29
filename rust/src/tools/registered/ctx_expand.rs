use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxExpandTool;

impl McpTool for CtxExpandTool {
    fn name(&self) -> &'static str {
        "ctx_expand"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_expand",
            "Retrieve archived tool output: see [Archived:ID] → ctx_expand id=ID \
             (zero-loss, original preserved). head/tail/search filter lines; \
             action=list|search_all browses/queries archives.\n\
             ANTIPATTERN: not for project files — use ctx_read or ctx_compose.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Archive ID, shell_* job ID, or @F1 ref" },
                    "action": { "type": "string", "description": "retrieve|list|search_all" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" },
                    "head": { "type": "integer" },
                    "tail": { "type": "integer" },
                    "search": { "type": "string" },
                    "json_keys": { "type": "boolean" },
                    "json_path": { "type": "string", "description": "e.g. data.items.0" },
                    "query": { "type": "string" },
                    "session_id": { "type": "string" }
                },
                "allOf": [
                    { "if": { "properties": { "action": { "const": "search_all" } }, "required": ["action"] }, "then": { "required": ["query"] } },
                    { "if": { "not": { "properties": { "action": { "enum": ["list", "search_all"] } }, "required": ["action"] } }, "then": { "required": ["id"] } }
                ]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let args_val = Value::Object(args.clone());
        // A failed retrieval is a tool error with the same text: it is never
        // recorded as a delivery, and it has published no lineage.
        let result = crate::tools::ctx_expand::handle_outcome(&args_val)
            .map_err(crate::server::tool_trait::tool_execution_error)?;
        Ok(ToolOutput::simple(result))
    }

    /// Declared for retrieval (every action but `list` and `search_all`):
    /// `ctx_expand::handle_retrieve` publishes the file lineage of the copy
    /// it resolved, and marks the capture incomplete for every store without
    /// one (tee, reference, public archive) and for a copy whose lineage
    /// names a caller-presented source — the capture carries files only, so
    /// such a reply stays unknown. A handle reference and a refusal publish
    /// nothing. The pipeline then records the reply as it is delivered.
    fn publishes_every_source(&self, args: Option<&Map<String, Value>>) -> bool {
        !matches!(
            args.and_then(|args| args.get("action"))
                .and_then(Value::as_str),
            Some("list" | "search_all")
        )
    }
}
