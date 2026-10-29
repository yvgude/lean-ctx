use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ShellOutcome, ToolContext, ToolOutput, get_int, get_str, require_resolved_path,
};
use crate::tool_defs::tool_def;

pub struct CtxExecuteTool;

impl McpTool for CtxExecuteTool {
    fn name(&self) -> &'static str {
        "ctx_execute"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_execute",
            "Run code in sandbox (11 languages) — use when conditionals, multi-line or cross-language transforms.\n\
             ANTIPATTERN: for simple one-liners, prefer ctx_shell (lower overhead, auto-compressed).\n\
             language=shell supports multi-line scripts but shares ctx_shell's security policy;\n\
             other languages run only after the user approves them.\n\
             action=code (default) for one-shot; action=batch for parallel multi-language;\n\
             action=file to summarize admitted project text as data, without executing it.\n\
             For code/batch, intent annotates large output; file records the intent alongside its preview. Languages: javascript,\n\
             typescript, python, shell, ruby, go, rust, php, perl, r, elixir.",
            json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "javascript|typescript|python|shell|ruby|go|rust|php|perl|r|elixir (for action=code)"
                    },
                    "code": {
                        "type": "string",
                        "description": "Source code for action=code. Set intent to filter large output."
                    },
                    "intent": {
                        "type": "string",
                        "description": "Focus intent; triggers filtering when output is large."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 30)"
                    },
                    "action": {
                        "type": "string",
                        "description": "code (default, run script) | batch (parallel) | file (project file)"
                    },
                    "items": {
                        "type": "string",
                        "description": "JSON array of [{language, code}] for batch action."
                    },
                    "path": {
                        "type": "string",
                        "description": "Project text file for action=file; content is inspected, never executed."
                    }
                },
                "oneOf": [
                    {
                        "properties": { "action": { "enum": ["code"] } },
                        "required": ["language", "code"]
                    },
                    {
                        "properties": { "action": { "const": "batch" } },
                        "required": ["action", "items"]
                    },
                    {
                        "properties": { "action": { "const": "file" } },
                        "required": ["action", "path"]
                    }
                ]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_default();

        // action=file inspects text and never executes it; code and batch run
        // code, so restricted clients are refused like ctx_shell refuses them.
        if action != "file"
            && let Some(message) = super::ctx_shell::shell_access_denial(ctx)
        {
            return Ok(ToolOutput {
                shell_outcome: Some(crate::server::tool_trait::ShellOutcome::Blocked),
                content_blocks: None,
                ..ToolOutput::simple(message)
            });
        }
        let (result, outcome) = if action == "batch" {
            let items_str = get_str(args, "items")
                .ok_or_else(|| ErrorData::invalid_params("items is required for batch", None))?;
            let items: Vec<serde_json::Value> = serde_json::from_str(&items_str)
                .map_err(|e| ErrorData::invalid_params(format!("Invalid items JSON: {e}"), None))?;
            let batch: Vec<(String, String)> = items
                .iter()
                .filter_map(|item| {
                    let lang = item.get("language")?.as_str()?.to_string();
                    let code = item.get("code")?.as_str()?.to_string();
                    Some((lang, code))
                })
                .collect();
            match crate::tools::ctx_execute::refuse_unapproved(
                batch.iter().map(|(language, _)| language.as_str()),
            ) {
                Ok(()) => crate::tools::ctx_execute::handle_batch(&batch),
                Err(message) => (message, ShellOutcome::Blocked),
            }
        } else if action == "file" {
            let path = require_resolved_path(ctx, args, "path")?;
            let project_root = if ctx.project_root.is_empty() {
                None
            } else {
                Some(ctx.project_root.as_str())
            };
            let intent = get_str(args, "intent");
            crate::tools::ctx_execute::handle_file(&path, intent.as_deref(), project_root)
        } else {
            let language = get_str(args, "language")
                .ok_or_else(|| ErrorData::invalid_params("language is required", None))?;
            let code = get_str(args, "code")
                .ok_or_else(|| ErrorData::invalid_params("code is required", None))?;
            let intent = get_str(args, "intent");
            let timeout = get_int(args, "timeout").and_then(|t| u64::try_from(t).ok());
            match crate::tools::ctx_execute::refuse_unapproved([language.as_str()]) {
                Ok(()) => {
                    crate::tools::ctx_execute::handle(&language, &code, intent.as_deref(), timeout)
                }
                Err(message) => (message, ShellOutcome::Blocked),
            }
        };

        let result = crate::core::redaction::redact_text_if_enabled(&result);
        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: Some(outcome),
            content_blocks: None,
        })
    }

    /// `action=file` reads exactly one source, through the admitted rooted
    /// reader on the handler thread (`tools::ctx_execute::handle_file_in_view`)
    /// and derives its summary from that text alone. `code` and `batch` run
    /// arbitrary programs whose reads nothing here observes: not declared.
    fn publishes_every_source(&self, args: Option<&Map<String, Value>>) -> bool {
        args.and_then(|args| args.get("action"))
            .and_then(Value::as_str)
            == Some("file")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tool_trait::ShellOutcome;

    fn args(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
            .collect()
    }

    /// GHSA-rxv5-4c3x-g4q6 B: clients that may not run ctx_shell may not run
    /// code through ctx_execute either.
    #[test]
    fn restricted_clients_cannot_execute_code() {
        let code = args(&[("language", "python"), ("code", "print(1)")]);
        let batch = args(&[
            ("action", "batch"),
            ("items", r#"[{"language":"python","code":"print(1)"}]"#),
        ]);
        let contexts = [
            ToolContext {
                client_role: Some("untrusted".to_string()),
                ..ToolContext::default()
            },
            ToolContext {
                client_role: Some("readonly".to_string()),
                ..ToolContext::default()
            },
            ToolContext {
                shell_access: Some(false),
                ..ToolContext::default()
            },
        ];
        for ctx in &contexts {
            for request in [&code, &batch] {
                let output = CtxExecuteTool
                    .handle(request, ctx)
                    .expect("denial is a tool result");
                assert_eq!(output.shell_outcome, Some(ShellOutcome::Blocked));
                assert!(
                    output.text.contains("SHELL ACCESS DENIED"),
                    "{}",
                    output.text
                );
            }
        }
    }
}
