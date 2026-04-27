use rmcp::ErrorData;
use serde_json::Value;

use crate::server::execute::execute_command_in;
use crate::server::helpers::{get_bool, get_int, get_str};
use crate::tools::LeanCtxServer;

impl LeanCtxServer {
    pub(crate) async fn dispatch_shell_tools(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<String, ErrorData> {
        Ok(match name {
            "ctx_shell" => {
                let command = get_str(args, "command")
                    .ok_or_else(|| ErrorData::invalid_params("command is required", None))?;

                if let Some(rejection) = crate::tools::ctx_shell::validate_command(&command) {
                    self.record_call("ctx_shell", 0, 0, None).await;
                    return Ok(rejection);
                }

                let explicit_cwd = get_str(args, "cwd");
                let effective_cwd = {
                    let session = self.session.read().await;
                    session.effective_cwd(explicit_cwd.as_deref())
                };

                let ensured_root = {
                    let mut session = self.session.write().await;
                    session.update_shell_cwd(&command);
                    let root_missing = session
                        .project_root
                        .as_deref()
                        .is_none_or(|r| r.trim().is_empty());
                    if root_missing {
                        let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string());
                        crate::core::protocol::detect_project_root(&effective_cwd).and_then(|r| {
                            if home.as_deref() == Some(r.as_str()) {
                                None
                            } else {
                                session.project_root = Some(r.clone());
                                Some(r)
                            }
                        })
                    } else {
                        None
                    }
                };
                if let Some(root) = ensured_root.as_deref() {
                    crate::core::index_orchestrator::ensure_all_background(root);
                    let mut current = self.agent_id.write().await;
                    if current.is_none() {
                        let mut registry = crate::core::agents::AgentRegistry::load_or_create();
                        registry.cleanup_stale(24);
                        let role = std::env::var("LEAN_CTX_AGENT_ROLE").ok();
                        let id = registry.register("mcp", role.as_deref(), root);
                        let _ = registry.save();
                        *current = Some(id);
                    }
                }

                let raw = get_bool(args, "raw").unwrap_or(false)
                    || std::env::var("LEAN_CTX_DISABLED").is_ok();
                let cmd_clone = command.clone();
                let cwd_clone = effective_cwd.clone();
                let crp_mode = self.crp_mode;

                let (result_out, original, saved, tee_hint) =
                    tokio::task::spawn_blocking(move || {
                        let (output, _real_exit_code) = execute_command_in(&cmd_clone, &cwd_clone);

                        if raw {
                            let tokens = crate::core::tokens::count_tokens(&output);
                            (output, tokens, 0, String::new())
                        } else {
                            let result =
                                crate::tools::ctx_shell::handle(&cmd_clone, &output, crp_mode);
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
                        }
                    })
                    .await
                    .unwrap_or_else(|e| {
                        (
                            format!("ERROR: shell task failed: {e}"),
                            0,
                            0,
                            String::new(),
                        )
                    });

                self.record_call("ctx_shell", original, saved, None).await;

                let savings_note = if !minimal && !raw && saved > 0 {
                    format!("\n[saved {saved} tokens vs native Shell]")
                } else {
                    String::new()
                };

                let shell_mismatch = if cfg!(windows) {
                    shell_mismatch_hint(&command, &result_out)
                } else {
                    String::new()
                };

                format!("{result_out}{savings_note}{tee_hint}{shell_mismatch}")
            }
            "ctx_search" => {
                let pattern = get_str(args, "pattern")
                    .ok_or_else(|| ErrorData::invalid_params("pattern is required", None))?;
                let path = self
                    .resolve_path(&get_str(args, "path").unwrap_or_else(|| ".".to_string()))
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let ext = get_str(args, "ext");
                let max = get_int(args, "max_results").unwrap_or(20) as usize;
                let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);
                let crp = self.crp_mode;
                let respect = !no_gitignore;
                let search_result = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    tokio::task::spawn_blocking(move || {
                        crate::tools::ctx_search::handle(
                            &pattern,
                            &path,
                            ext.as_deref(),
                            max,
                            crp,
                            respect,
                        )
                    }),
                )
                .await;
                let (result, original) = match search_result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        return Err(ErrorData::internal_error(
                            format!("search task failed: {e}"),
                            None,
                        ))
                    }
                    Err(_) => {
                        let msg = "ctx_search timed out after 30s. Try narrowing the search:\n\
                                   • Use a more specific pattern\n\
                                   • Specify ext= to limit file types\n\
                                   • Specify a subdirectory in path=";
                        self.record_call("ctx_search", 0, 0, None).await;
                        return Ok(msg.to_string());
                    }
                };
                let sent = crate::core::tokens::count_tokens(&result);
                let saved = original.saturating_sub(sent);
                self.record_call("ctx_search", original, saved, None).await;
                let savings_note = if !minimal && saved > 0 {
                    format!("\n[saved {saved} tokens vs native Grep]")
                } else {
                    String::new()
                };
                format!("{result}{savings_note}")
            }
            "ctx_execute" => {
                let action = get_str(args, "action").unwrap_or_default();

                let result = if action == "batch" {
                    let items_str = get_str(args, "items").ok_or_else(|| {
                        ErrorData::invalid_params("items is required for batch", None)
                    })?;
                    let items: Vec<serde_json::Value> =
                        serde_json::from_str(&items_str).map_err(|e| {
                            ErrorData::invalid_params(format!("Invalid items JSON: {e}"), None)
                        })?;
                    let batch: Vec<(String, String)> = items
                        .iter()
                        .filter_map(|item| {
                            let lang = item.get("language")?.as_str()?.to_string();
                            let code = item.get("code")?.as_str()?.to_string();
                            Some((lang, code))
                        })
                        .collect();
                    crate::tools::ctx_execute::handle_batch(&batch)
                } else if action == "file" {
                    let raw_path = get_str(args, "path").ok_or_else(|| {
                        ErrorData::invalid_params("path is required for action=file", None)
                    })?;
                    let path = self.resolve_path(&raw_path).await.map_err(|e| {
                        ErrorData::invalid_params(format!("path rejected: {e}"), None)
                    })?;
                    let intent = get_str(args, "intent");
                    crate::tools::ctx_execute::handle_file(&path, intent.as_deref())
                } else {
                    let language = get_str(args, "language")
                        .ok_or_else(|| ErrorData::invalid_params("language is required", None))?;
                    let code = get_str(args, "code")
                        .ok_or_else(|| ErrorData::invalid_params("code is required", None))?;
                    let intent = get_str(args, "intent");
                    let timeout = get_int(args, "timeout").map(|t| t as u64);
                    crate::tools::ctx_execute::handle(&language, &code, intent.as_deref(), timeout)
                };

                self.record_call("ctx_execute", 0, 0, Some(action)).await;
                result
            }
            _ => unreachable!("dispatch_shell_tools called with unknown tool: {name}"),
        })
    }
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
