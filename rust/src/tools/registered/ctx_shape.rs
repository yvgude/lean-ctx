use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

/// Shapes the output of a host-native tool *after* it ran (concept K2,
/// "shape, don't redirect"): a host mod (Claude Code `tool.call`) forwards the
/// native Bash stdout here instead of denying the call and forcing a round
/// trip to `ctx_shell`. The text it rewrites gets lean-ctx's secret redaction,
/// and the result flows through the same dispatch pipeline as every `ctx_*`
/// output (sensitivity floor, policy-pack redaction, input filters).
///
/// Host-only surface: a host extension calls it directly (`$.mcp.call`); it is
/// never advertised and refused through `ctx_call` — see
/// `dynamic_tools::INTERNAL_HOST_TOOLS`.
pub struct CtxShapeTool;

/// Below this many **bytes** the shaping gain cannot pay for the call. Bytes,
/// not chars: the Claude Code mod measures JavaScript string length (UTF-16
/// units), and a text is never fewer bytes than UTF-16 units — so anything the
/// mod sends clears this floor instead of slipping back unredacted.
const MIN_SHAPE_BYTES: usize = 2_000;
/// Above this the input is returned unchanged: bounds memory and CPU for a
/// single call (native outputs that large are persisted by the host anyway).
const MAX_SHAPE_BYTES: usize = 8 * 1024 * 1024;

impl McpTool for CtxShapeTool {
    fn name(&self) -> &'static str {
        "ctx_shape"
    }

    /// The body replaces a native tool's output verbatim, so the pipeline must
    /// hand it back without hints, nudges or checkpoints — after its redaction,
    /// sensitivity and filter passes, which machine-readable calls keep.
    fn produces_machine_readable(&self, _args: Option<&Map<String, Value>>) -> bool {
        true
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_shape",
            "Internal host hook: shape a native tool's output after it ran. Not for agents.",
            json!({
                "type": "object",
                "properties": {
                    "tool": { "type": "string", "description": "Native tool name, e.g. Bash" },
                    "command": { "type": "string", "description": "Exact command line (Bash)" },
                    "exit_code": { "type": "integer", "description": "The command's exit code, when known" },
                    "output": { "type": "string", "description": "The native tool's output as the model would read it" }
                },
                "required": ["tool", "output"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let tool = args
            .get("tool")
            .and_then(Value::as_str)
            .ok_or_else(|| ErrorData::invalid_params("tool is required", None))?;
        let output = args
            .get("output")
            .and_then(Value::as_str)
            .ok_or_else(|| ErrorData::invalid_params("output is required", None))?;
        let command = args.get("command").and_then(Value::as_str);
        let exit_code = args
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|c| i32::try_from(c).ok());

        let shaped = shape(tool, command, exit_code, output);
        let original_tokens = crate::core::tokens::count_tokens(output);
        let shaped_tokens = crate::core::tokens::count_tokens(&shaped);
        Ok(ToolOutput {
            text: shaped,
            original_tokens,
            saved_tokens: original_tokens.saturating_sub(shaped_tokens),
            mode: Some("shape".to_string()),
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}

/// Pure shaping decision, separated from MCP plumbing for testing.
fn shape(tool: &str, command: Option<&str>, exit_code: Option<i32>, output: &str) -> String {
    if output.len() < MIN_SHAPE_BYTES || output.len() > MAX_SHAPE_BYTES {
        return output.to_string();
    }
    let redacted = super::ctx_shell_background::redact_shell_output_secrets(output);
    let shaped = match (tool, command) {
        ("Bash", Some(cmd)) if !cmd.trim().is_empty() => {
            crate::proxy::compress::shape_command_output(cmd, &redacted, exit_code)
        }
        _ => crate::proxy::compress::compress_tool_result(&redacted, Some(tool)),
    };
    // Never hand back more than the host already has.
    if shaped.len() >= redacted.len() {
        redacted
    } else {
        shaped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_the_size_window_output_is_returned_verbatim() {
        assert_eq!(shape("Bash", Some("ls"), Some(0), "a\nb\n"), "a\nb\n");
        let huge = "x\n".repeat(MAX_SHAPE_BYTES / 2 + 1);
        assert_eq!(shape("Bash", Some("cat big"), Some(0), &huge), huge);
    }

    /// A successful `cargo build` (real shape: progress lines + `Finished`)
    /// must shrink on the success path and stay recoverable — never larger
    /// than the input.
    #[test]
    fn successful_build_output_is_folded_and_recoverable() {
        use std::fmt::Write as _;
        let mut noisy = String::new();
        for i in 0..400 {
            let _ = writeln!(noisy, "   Compiling crate-{i} v0.1.{i} (/tmp/x)");
        }
        noisy
            .push_str("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 42.17s\n");
        let shaped = shape("Bash", Some("cargo build"), Some(0), &noisy);
        assert!(
            shaped.len() < noisy.len(),
            "verbose cargo output must shrink"
        );
        assert!(
            shaped.contains("Finished"),
            "the build verdict must survive: {shaped}"
        );
        assert!(
            shaped.contains("full original"),
            "a lossy shrink must carry its recovery handle: {shaped}"
        );

        let mut opaque = String::new();
        for i in 0..300 {
            let _ = writeln!(opaque, "{i:x}{}", i * 7919);
        }
        assert!(shape("Grep", None, None, &opaque).len() <= opaque.len());
    }
}
