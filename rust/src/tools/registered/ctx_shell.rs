use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ShellOutcome, ToolContext, ToolOutput, get_bool, get_int, get_str,
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
            "WORKFLOW: preferred — auto-compresses output (build/test/log).\n\
             raw=true for verbatim output.\n\
             [exit:N] on errors (lossless).\n\
             POLICY (by design): allowlisted read-only path; ctx_execute is the trusted script path.\n\
             A [BLOCKED] command is permanent — escalate to ctx_execute(language=\"shell\"), do not retry here.\n\
             ANTIPATTERN: multi-line scripts, sh/bash script.sh, $var-as-command → ctx_execute.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command" },
                    "raw": { "type": "boolean", "description": "Skip compression (verbatim)" },
                    "cwd": { "type": "string", "description": "Working dir (persists across calls)" },
                    "timeout_ms": { "type": "integer", "description": "Per-call timeout in ms (max 3600000). Overridden by LEAN_CTX_SHELL_TIMEOUT_MS." },
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
        let timeout_ms = get_int(args, "timeout_ms").and_then(|n| u64::try_from(n).ok());

        // The write-doctrine check (no `>`, `tee`, heredoc-to-file, curl -o, …)
        // is an MCP-payload-safety convention, not a security boundary, so it is
        // opt-out via `shell_allow_writes` (#523). The real command gating
        // (`check_shell_allowlist`, below) is NOT affected by this flag.
        if !crate::core::config::Config::load().shell_allow_writes_effective()
            && let Some(rejection) = crate::tools::ctx_shell::validate_command(&command)
        {
            // The command never ran — report as a tool error so MCP clients
            // (guards, retry logic) can detect it programmatically (#389).
            return Ok(ToolOutput {
                shell_outcome: Some(ShellOutcome::Blocked),
                content_blocks: None,
                ..ToolOutput::simple(rejection)
            });
        }

        if let Err(msg) = crate::core::shell_allowlist::check_shell_allowlist(&command) {
            return Ok(ToolOutput {
                shell_outcome: Some(ShellOutcome::Blocked),
                content_blocks: None,
                ..ToolOutput::simple(msg.to_string())
            });
        }

        warn_shell_secret_paths(&command);

        // #842: a bare `cat <file>` is better served by ctx_read — it delivers
        // content inline instead of firewalling/archiving the output, avoiding
        // a mandatory ctx_expand round-trip for agents with cat-muscle-memory.
        if let Some(read_path) = detect_bare_cat_file(&command)
            && let Some(cache_lock) = ctx.cache.as_ref()
            && let Some(mut cache) = crate::server::bounded_lock::write(cache_lock, "cat_redirect")
        {
            let result = crate::tools::ctx_read::handle_with_task_resolved(
                &mut cache,
                &read_path,
                "full",
                crate::tools::CrpMode::Off,
                None,
            );
            let note = format!(
                "\n[ctx_shell: bare `cat` redirected to ctx_read for inline delivery. \
                         Use ctx_read(path=\"{read_path}\") directly next time.]"
            );
            let out = format!("{}{note}", result.content);
            let sent = crate::core::tokens::count_tokens(&out);
            return Ok(ToolOutput {
                text: out,
                original_tokens: sent,
                saved_tokens: 0,
                mode: Some("cat-redirect".to_string()),
                path: Some(read_path),
                changed: false,
                shell_outcome: Some(ShellOutcome::Exit(0)),
                content_blocks: None,
            });
        }

        tokio::task::block_in_place(|| {
            let session_lock = ctx
                .session
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("session not available", None))?;

            let explicit_cwd = get_str(args, "cwd");
            let had_explicit_cwd = explicit_cwd.is_some();
            let (effective_cwd, cwd_jail_reason) = {
                let guard = crate::server::bounded_lock::read(session_lock, "ctx_shell_cwd");
                match guard {
                    Some(session) => session.effective_cwd_checked(explicit_cwd.as_deref()),
                    None => (explicit_cwd.unwrap_or_else(|| ".".to_string()), None),
                }
            };
            // A `cwd` rejected by the project-root jail is silently replaced with
            // the root (deliberate sandboxing). Surface that swap as a one-line
            // hint so the caller does not mistake the run dir for the requested
            // one (#629); appended at the end of the output like the other hints.
            let cwd_jail_reason_was_none = cwd_jail_reason.is_none();
            let cwd_jail_hint = cwd_jail_reason.map_or_else(String::new, |reason| {
                format!(
                    "\n[cwd: requested path rejected by project-root jail ({reason}) \u{2014} ran in {effective_cwd} instead]"
                )
            });

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
                        &cmd_clone, &cwd_clone, &extra_env, timeout_ms,
                    );
                    let output = redact_shell_output_secrets(&raw_output);
                    // Keep failure reporting consistent on this degraded path:
                    // same [exit:N] footer and the same structured outcome (#389).
                    let exit_suffix = match exit_code {
                        0 => String::new(),
                        124 => "\n[exit:124 — command timed out]".to_string(),
                        _ => format!("\n[exit:{exit_code}]"),
                    };
                    return Ok(ToolOutput {
                        shell_outcome: Some(ShellOutcome::Exit(exit_code)),
                        content_blocks: None,
                        ..ToolOutput::simple(format!("{output}{exit_suffix}"))
                    });
                };
                // #707: a jail-accepted explicit `cwd` param is the client
                // telling us where it now works (worktree switches arrive
                // this way, not as `cd` commands) — persist it so path
                // resolution's divergence check tracks the live checkout.
                if had_explicit_cwd && cwd_jail_reason_was_none {
                    session.note_explicit_cwd(&effective_cwd);
                }
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
                &cmd_clone, &cwd_clone, &extra_env, timeout_ms,
            );

            // Structured diagnostics (#499) — same hook as the CLI path.
            crate::core::diagnostics_store::record_from_shell(&cmd_clone, &raw_output, exit_code);

            let output = redact_shell_output_secrets(&raw_output);

            let (result_out, original, saved, tee_hint) = if raw {
                let tokens = crate::core::tokens::count_tokens(&output);
                (output, tokens, 0, String::new())
            } else {
                let _mode_guard = crate::core::savings_footer::ModeGuard::new("shell");
                let result =
                    crate::tools::ctx_shell::handle(&cmd_clone, &output, exit_code, crp_mode);
                let original = crate::core::tokens::count_tokens(&output);
                let sent = crate::core::tokens::count_tokens(&result);
                let saved = original.saturating_sub(sent);

                let cfg = crate::core::config::Config::load();
                // Shared tee policy (#811): identical decision to the CLI path —
                // `Failures` keys off the real exit code, not a substring match.
                let tee_hint = if crate::shell::tee_policy::should_tee(
                    &cfg.tee_mode,
                    exit_code,
                    output.trim().is_empty(),
                    original,
                    sent,
                ) {
                    crate::shell::save_tee(&cmd_clone, &output)
                        .map(|p| {
                            if matches!(cfg.tee_mode, crate::core::config::TeeMode::HighCompression)
                            {
                                let pct = crate::shell::tee_policy::savings_pct(original, sent);
                                // Recovery grammar (path-first, MCP-optional): the raw
                                // bytes are a real file the agent can read with any tool
                                // (no MCP needed) — for orgs that forbid it — and the same
                                // path doubles as the ctx_expand id for surgical slices
                                // (head/search/json_path) without re-reading it all (#936).
                                format!(
                                    "\n[compressed {pct:.0}%: full output at {p} — read it directly (no MCP), or ctx_expand(id=\"{p}\", search=\"…\"|head=N|json_path=\"…\") for a slice]"
                                )
                            } else {
                                format!("\n[full output: {p} — read it directly (no MCP), or ctx_expand(id=\"{p}\")]")
                            }
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
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
            // #815: exit 124 = timeout signal (from timeout(1) / lean-ctx
            // shell timeout). Make it explicit so agents don't confuse a
            // timed-out command with a successful empty result.
            let exit_suffix = match exit_code {
                0 => String::new(),
                124 => "\n[exit:124 — command timed out]".to_string(),
                _ => format!("\n[exit:{exit_code}]"),
            };
            let nudge = if raw { "" } else { search_tool_nudge(&command) };
            let final_out = format!(
                "{result_out}{tee_hint}{shell_mismatch}{cwd_jail_hint}{nudge}{exit_suffix}"
            );

            Ok(ToolOutput {
                text: final_out,
                original_tokens: original,
                saved_tokens: saved,
                mode,
                path: None,
                changed: false,
                shell_outcome: Some(ShellOutcome::Exit(exit_code)),
                content_blocks: None,
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

fn search_tool_nudge(command: &str) -> &'static str {
    let cmd = command.trim();
    let first_word = cmd.split_whitespace().next().unwrap_or("");
    if !cmd.contains('|') {
        match first_word {
            "grep" | "rg" | "egrep" | "fgrep" | "ag" => {
                return "\n[hint: use ctx_search for structured, cached results with symbol/semantic modes]";
            }
            "find" => {
                return "\n[hint: use ctx_glob or ctx_tree for structured file discovery]";
            }
            "ls" | "exa" | "eza" => {
                return "\n[hint: use ctx_tree for structured directory listing]";
            }
            _ => {}
        }
    }
    ""
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

/// #842: detect a bare `cat <single_file>` command (no pipes, redirects, flags).
fn detect_bare_cat_file(command: &str) -> Option<String> {
    let trimmed = command.trim();
    let rest = trimmed.strip_prefix("cat ")?;
    let rest = rest.trim();
    if rest.is_empty()
        || rest.contains('|')
        || rest.contains('>')
        || rest.contains('<')
        || rest.contains(';')
        || rest.contains('&')
        || rest.contains('$')
        || rest.starts_with('-')
    {
        return None;
    }
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 1 {
        return None;
    }
    let file_path = parts[0].trim_matches(|c: char| c == '\'' || c == '"');
    if file_path.is_empty() {
        return None;
    }
    Some(file_path.to_string())
}
