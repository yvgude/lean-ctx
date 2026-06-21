use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

/// A `shell` tool alias that transparently delegates to `ctx_shell`'s compression
/// logic. Registered for all MCP clients (see `server::registry`); it exists for
/// clients (like Codex Desktop) whose agent model prefers a tool named `shell` /
/// `bash` over `ctx_shell` and would otherwise fall back to a native, uncompressed
/// shell tool.
///
/// This solves the "Codex Desktop doesn't compress" issue (#337): the Desktop app
/// loads the MCP server but the agent ignores `ctx_shell` and uses its native
/// `Bash` tool instead. By providing a `shell` tool with a familiar interface,
/// the model naturally routes commands through our compression pipeline.
pub struct ShellAliasTool;

impl McpTool for ShellAliasTool {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "shell",
            "Shell command with auto-compression (~95 patterns). Alias for ctx_shell.\n\
             Output is compressed for token savings. For verbatim output pass raw=true.",
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working dir (default: project root)"
                    }
                },
                "required": ["command"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let command = get_str(args, "command")
            .ok_or_else(|| ErrorData::invalid_params("command is required", None))?;

        if let Some(rejection) = crate::tools::ctx_shell::validate_command(&command) {
            return Ok(ToolOutput::simple(rejection));
        }

        if let Err(msg) = crate::core::shell_allowlist::check_shell_allowlist(&command) {
            return Ok(ToolOutput::simple(msg));
        }

        tokio::task::block_in_place(|| {
            let cwd = get_str(args, "cwd");
            let mut shell_args = Map::new();
            shell_args.insert("command".to_string(), Value::String(command));
            if let Some(dir) = cwd {
                shell_args.insert("cwd".to_string(), Value::String(dir));
            }
            // raw=false → always compress (the whole point of this alias)
            shell_args.insert("raw".to_string(), Value::Bool(false));

            crate::tools::registered::ctx_shell::CtxShellTool.handle(&shell_args, ctx)
        })
    }
}
