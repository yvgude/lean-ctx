use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxSkillifyTool;

impl McpTool for CtxSkillifyTool {
    fn name(&self) -> &'static str {
        "ctx_skillify"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_skillify",
            "Codify recurring patterns from this project's session diary + knowledge into versioned, git-committable .cursor/rules/skillify-*.mdc files. Actions: mine (distill & write/merge rules), list (show generated rules), status (config + counts), promote (copy a project rule to ~/.cursor/rules). Precision-biased; only acts when invoked; re-runs are idempotent.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["mine", "list", "status", "promote"],
                        "description": "Skillify action (default: mine)"
                    },
                    "slug": {
                        "type": "string",
                        "description": "Rule slug for the promote action (e.g. skillify-stop-before-build)"
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
        let action = get_str(args, "action").unwrap_or_else(|| "mine".to_string());
        let slug = get_str(args, "slug");
        let result =
            crate::tools::ctx_skillify::handle(&ctx.project_root, &action, slug.as_deref());
        Ok(ToolOutput::simple(result))
    }
}
