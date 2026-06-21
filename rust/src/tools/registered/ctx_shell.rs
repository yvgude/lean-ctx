use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ShellOutcome, ToolContext, ToolOutput, get_bool, get_str,
};
use crate::tool_defs::tool_def;

pub struct CtxShellTool;

impl McpTool for CtxShellTool {
    fn name(&self) -> &'static str {
        "ctx_shell"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_shell",
            "Run shell commands with automatic output compression (~95 patterns).\n\
             Optimized for build/test/log output (cargo, npm, pytest, go test).\n\
             raw=true disables compression for verbatim output. Lossless for errors\n\
             and exit codes — [exit:N] footer for failure codes. cwd persists.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command" },
                    "raw": { "type": "boolean", "description": "Skip compression (verbatim)" },
                    "cwd": { "type": "string", "description": "Working dir (default: last cd or project root)" },
                    "env": { "type": "object", "description": "Extra env vars", "additionalProperties": { "type": "string" } }
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
            // The command never ran — report as a tool error so MCP clients
            // (guards, retry logic) can detect it programmatically (#389).
            return Ok(ToolOutput {
                shell_outcome: Some(ShellOutcome::Blocked),
                ..ToolOutput::simple(rejection)
            });
        }

        if let Err(msg) = crate::core::shell_allowlist::check_shell_allowlist(&command) {
            return Ok(ToolOutput {
                shell_outcome: Some(ShellOutcome::Blocked),
                ..ToolOutput::simple(msg)
            });
        }

        warn_shell_secret_paths(&command);

        tokio::task::block_in_place(|| {
            let session_lock = ctx
                .session
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("session not available", None))?;

            let explicit_cwd = get_str(args, "cwd");
            let effective_cwd = {
                let guard = crate::server::bounded_lock::read(session_lock, "ctx_shell_cwd");
                match guard {
                    Some(session) => session.effective_cwd(explicit_cwd.as_deref()),
                    None => explicit_cwd.unwrap_or_else(|| ".".to_string()),
                }
            };

            {
                let Some(mut session) =
                    crate::server::bounded_lock::write(session_lock, "ctx_shell_write")
                else {
                    tracing::debug!("[ctx_shell: session lock timeout, proceeding without update]");
                    let cmd_clone = command.clone();
                    let cwd_clone = effective_cwd.clone();
                    let extra_env: std::collections::HashMap<String, String> = args
                        .get("env")
                        .and_then(|v| v.as_object())
                        .map(|obj| {
                            obj.iter()
                                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                                .filter(|(k, _)| !is_dangerous_env_key(k))
                                .collect()
                        })
                        .unwrap_or_default();
                    let (raw_output, exit_code) = crate::server::execute::execute_command_with_env(
                        &cmd_clone, &cwd_clone, &extra_env,
                    );
                    let output = redact_shell_output_secrets(&raw_output);
                    // Keep failure reporting consistent on this degraded path:
                    // same [exit:N] footer and the same structured outcome (#389).
                    let exit_suffix = if exit_code != 0 {
                        format!("\n[exit:{exit_code}]")
                    } else {
                        String::new()
                    };
                    return Ok(ToolOutput {
                        shell_outcome: Some(ShellOutcome::Exit(exit_code)),
                        ..ToolOutput::simple(format!("{output}{exit_suffix}"))
                    });
                };
                session.update_shell_cwd(&command);
                let root_missing = session
                    .project_root
                    .as_deref()
                    .is_none_or(|r| r.trim().is_empty());
                if root_missing {
                    let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string());
                    if let Some(root) = crate::core::protocol::detect_project_root(&effective_cwd)
                        && home.as_deref() != Some(root.as_str())
                    {
                        session.project_root = Some(root.clone());
                        crate::core::index_orchestrator::ensure_all_background(&root);
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

            let extra_env: std::collections::HashMap<String, String> = args
                .get("env")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .filter(|(k, _)| !is_dangerous_env_key(k))
                        .collect()
                })
                .unwrap_or_default();

            let (raw_output, exit_code) = crate::server::execute::execute_command_with_env(
                &cmd_clone, &cwd_clone, &extra_env,
            );

            // Structured diagnostics (#499) — same hook as the CLI path.
            crate::core::diagnostics_store::record_from_shell(&cmd_clone, &raw_output, exit_code);

            let output = redact_shell_output_secrets(&raw_output);

            let (result_out, original, saved, tee_hint) = if raw {
                let tokens = crate::core::tokens::count_tokens(&output);
                (output, tokens, 0, String::new())
            } else {
                let _mode_guard = crate::core::savings_footer::ModeGuard::new("shell");
                let result = crate::tools::ctx_shell::handle(&cmd_clone, &output, crp_mode);
                let original = crate::core::tokens::count_tokens(&output);
                let sent = crate::core::tokens::count_tokens(&result);
                let saved = original.saturating_sub(sent);

                let cfg = crate::core::config::Config::load();
                let savings_pct = if original > 0 {
                    ((original.saturating_sub(sent)) as f64 / original as f64) * 100.0
                } else {
                    0.0
                };
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
                    crate::core::config::TeeMode::HighCompression
                        if savings_pct > 70.0 && original > 100 =>
                    {
                        crate::shell::save_tee(&cmd_clone, &output)
                            .map(|p| {
                                format!(
                                    "\n[compressed {savings_pct:.0}%: full output at {p} if needed]"
                                )
                            })
                            .unwrap_or_default()
                    }
                    _ => {
                        if savings_pct > 70.0
                            && original > 100
                            && matches!(cfg.tee_mode, crate::core::config::TeeMode::Failures)
                        {
                            crate::shell::save_tee(&cmd_clone, &output)
                                .map(|p| format!("\n[compressed {savings_pct:.0}%: full output at {p} if needed]"))
                                .unwrap_or_default()
                        } else {
                            String::new()
                        }
                    }
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
            let exit_suffix = if exit_code != 0 {
                format!("\n[exit:{exit_code}]")
            } else {
                String::new()
            };
            let final_out = format!("{result_out}{tee_hint}{shell_mismatch}{exit_suffix}");

            Ok(ToolOutput {
                text: final_out,
                original_tokens: original,
                saved_tokens: saved,
                mode,
                path: None,
                changed: false,
                shell_outcome: Some(ShellOutcome::Exit(exit_code)),
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

fn is_dangerous_env_key(key: &str) -> bool {
    const BLOCKED: &[&str] = &[
        // Dynamic linker injection
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
        "DYLD_FRAMEWORK_PATH",
        // Shell re-entry / startup injection
        "BASH_ENV",
        "ENV",
        "PROMPT_COMMAND",
        "SHELL",
        "IFS",
        "CDPATH",
        // Binary resolution hijacking
        "PATH",
        "GIT_EXEC_PATH",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        // Identity / home directory manipulation
        "HOME",
        "USER",
        "LOGNAME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        // Language runtime search path hijacking
        "PYTHONPATH",
        "PYTHONSTARTUP",
        "PYTHONHOME",
        "NODE_PATH",
        "NODE_OPTIONS",
        "RUBYOPT",
        "RUBYLIB",
        "GEM_PATH",
        "GEM_HOME",
        "PERL5LIB",
        "PERL5OPT",
        "CLASSPATH",
        "JAVA_HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "GOPATH",
        "GOROOT",
    ];
    let upper = key.to_uppercase();
    if BLOCKED.contains(&upper.as_str()) {
        return true;
    }
    if upper.starts_with("LD_") && upper.ends_with("_PATH") {
        return true;
    }
    // Block all lean-ctx config overrides from env
    if upper.starts_with("LEAN_CTX_") || upper.starts_with("LCTX_") {
        return true;
    }
    false
}

/// Warn when shell reads secret-like paths via cat/head/tail/less/more.
/// WARN-ONLY: command still executes, this is purely observational.
fn warn_shell_secret_paths(command: &str) {
    const READ_CMDS: &[&str] = &["cat", "head", "tail", "less", "more", "bat"];
    let segments = crate::core::shell_allowlist::extract_all_commands_pub(command);
    for seg in &segments {
        let trimmed = seg.trim();
        let tokens = crate::core::shell_allowlist::shell_tokenize(trimmed);
        if tokens.is_empty() {
            continue;
        }
        let base = tokens[0]
            .rsplit('/')
            .next()
            .unwrap_or(&tokens[0])
            .to_string();
        if !READ_CMDS.contains(&base.as_str()) {
            continue;
        }
        for tok in &tokens[1..] {
            if tok.starts_with('-') {
                continue;
            }
            let path = std::path::Path::new(tok.as_str());
            if crate::core::io_boundary::is_secret_like(path).is_some() {
                tracing::warn!(
                    "[SECURITY] Shell reading secret-like path: {tok} (command: {base})"
                );
            }
        }
    }
}

/// Scans shell output for secrets and redacts them before returning to the agent.
fn redact_shell_output_secrets(output: &str) -> String {
    let cfg = crate::core::config::Config::load();
    if !cfg.secret_detection.enabled {
        return output.to_string();
    }
    let (redacted, matches) =
        crate::core::secret_detection::scan_and_redact(output, &cfg.secret_detection);
    if !matches.is_empty() {
        let names: Vec<&str> = matches.iter().map(|m| m.pattern_name).collect();
        tracing::warn!(
            "[SHELL SECRET REDACTION] {} secret(s) redacted from shell output: {}",
            matches.len(),
            names.join(", ")
        );
    }
    redacted
}
