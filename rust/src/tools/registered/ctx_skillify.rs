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
            "Codify recurring patterns from session diary + knowledge into versioned,\n\
             git-committable .cursor/rules/skillify-*.mdc files.\n\
             Actions: mine (distill & write/merge rules), list (show generated rules),\n\
             status (config + counts), promote (copy to ~/.cursor/rules).\n\
             Precision-biased; re-runs are idempotent.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["mine", "list", "status", "promote"],
                        "description": "mine|list|status|promote"
                    },
                    "slug": {
                        "type": "string",
                        "description": "Rule slug (for promote)"
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
