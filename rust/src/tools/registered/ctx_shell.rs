use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::server::tool_trait::{get_bool, get_str, McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxShellTool;

impl McpTool for CtxShellTool {
    fn name(&self) -> &'static str {
        "ctx_shell"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_shell",
            "Run shell command (compressed output, 95+ patterns). Use raw=true to skip compression. cwd sets working directory (persists across calls via cd tracking). Output redaction is on by default for non-admin roles (admin can disable).",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "raw": { "type": "boolean", "description": "Skip compression, return full uncompressed output. Redaction still applies by default for non-admin roles." },
                    "cwd": { "type": "string", "description": "Working directory for the command. If omitted, uses last cd target or project root." }
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

        tokio::task::block_in_place(|| {
            let session_lock = ctx
                .session
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("session not available", None))?;

            let explicit_cwd = get_str(args, "cwd");
            let effective_cwd = {
                let session = session_lock.blocking_read();
                session.effective_cwd(explicit_cwd.as_deref())
            };

            {
                let mut session = session_lock.blocking_write();
                session.update_shell_cwd(&command);
                let root_missing = session
                    .project_root
                    .as_deref()
                    .is_none_or(|r| r.trim().is_empty());
                if root_missing {
                    let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string());
                    if let Some(root) = crate::core::protocol::detect_project_root(&effective_cwd) {
                        if home.as_deref() != Some(root.as_str()) {
                            session.project_root = Some(root.clone());
                            crate::core::index_orchestrator::ensure_all_background(&root);
                        }
                    }
                }
            }

            let arg_raw = get_bool(args, "raw").unwrap_or(false);
            let arg_bypass = get_bool(args, "bypass").unwrap_or(false);
            let env_disabled = std::env::var("LEAN_CTX_DISABLED").is_ok();
            let env_raw = std::env::var("LEAN_CTX_RAW").is_ok();
            let (raw, bypass) = resolve_shell_raw_flags(arg_raw, arg_bypass, env_disabled, env_raw);

            let crp_mode = ctx.crp_mode;
            let cmd_clone = command.clone();
            let cwd_clone = effective_cwd;

            let (output, _exit_code) =
                crate::server::execute::execute_command_in(&cmd_clone, &cwd_clone);

            let (result_out, original, saved, tee_hint) = if raw {
                let tokens = crate::core::tokens::count_tokens(&output);
                (output, tokens, 0, String::new())
            } else {
                let result = crate::tools::ctx_shell::handle(&cmd_clone, &output, crp_mode);
                let original = crate::core::tokens::count_tokens(&output);
                let sent = crate::core::tokens::count_tokens(&result);
                let saved = original.saturating_sub(sent);

                let cfg = crate::core::config::Config::load();
                let tee_hint = match cfg.tee_mode {
                    crate::core::config::TeeMode::Always => {
                        crate::shell::save_tee(&cmd_clone, &output)
                            .map(|p| format!("\n[full output: {p}]"))
                            .unwrap_or_default()
                    }
                    crate::core::config::TeeMode::Failures
                        if !output.trim().is_empty()
                            && (output.contains("error")
                                || output.contains("Error")
                                || output.contains("ERROR")) =>
                    {
                        crate::shell::save_tee(&cmd_clone, &output)
                            .map(|p| format!("\n[full output: {p}]"))
                            .unwrap_or_default()
                    }
                    _ => String::new(),
                };

                (result, original, saved, tee_hint)
            };

            let mode = if bypass {
                Some("bypass".to_string())
            } else if raw {
                Some("raw".to_string())
            } else {
                None
            };

            let shell_mismatch = if cfg!(windows) && !raw {
                shell_mismatch_hint(&command, &result_out)
            } else {
                String::new()
            };

            let result_out = crate::core::redaction::redact_text_if_enabled(&result_out);
            let final_out = format!("{result_out}{tee_hint}{shell_mismatch}");

            Ok(ToolOutput {
                text: final_out,
                original_tokens: original,
                saved_tokens: saved,
                mode,
                path: None,
            })
        })
    }
}

#[allow(clippy::fn_params_excessive_bools)]
fn resolve_shell_raw_flags(
    arg_raw: bool,
    arg_bypass: bool,
    env_disabled: bool,
    env_raw: bool,
) -> (bool, bool) {
    let bypass = arg_bypass || env_raw;
    let raw = arg_raw || bypass || env_disabled;
    (raw, bypass)
}

fn shell_mismatch_hint(command: &str, output: &str) -> String {
    let shell = crate::shell::shell_name();
    let is_posix = matches!(shell.as_str(), "bash" | "sh" | "zsh" | "fish");
    let has_error = output.contains("is not recognized")
        || output.contains("not found")
        || output.contains("command not found");

    if !has_error {
        return String::new();
    }

    let powershell_cmds = [
        "Get-Content",
        "Select-Object",
        "Get-ChildItem",
        "Set-Location",
        "Where-Object",
        "ForEach-Object",
        "Select-String",
        "Invoke-Expression",
        "Write-Output",
    ];
    let uses_powershell = powershell_cmds
        .iter()
        .any(|c| command.contains(c) || command.contains(&c.to_lowercase()));

    if is_posix && uses_powershell {
        format!(
            "\n[shell: {shell} — use POSIX commands (cat, head, grep, find, ls) not PowerShell cmdlets]"
        )
    } else {
        String::new()
    }
}
