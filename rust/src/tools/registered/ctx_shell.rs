use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::ocla::cache_types::{CacheKeyBuilder, ShellCommandKey};

use crate::server::tool_trait::{
    BackgroundDisplay, BackgroundJobState, BackgroundLookupError, BackgroundShellOutcome, McpTool,
    ShellOutcome, ToolContext, ToolOutput, get_bool, get_int, get_str,
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
              raw=true for verbatim output; inline=true for moderately-sized verbatim output.\n\
             [exit:N] on errors (lossless).\n\
             POLICY (by design): allowlisted read-only path; ctx_execute is the trusted script path.\n\
             A [BLOCKED] command is permanent — escalate to ctx_execute(language=\"shell\"), do not retry here.\n\
             ANTIPATTERN: multi-line scripts, sh/bash script.sh, $var-as-command → ctx_execute.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command" },
                    "raw": { "type": "boolean", "description": "Skip compression (verbatim)" },
                    "inline": { "type": "boolean", "description": "Return verbatim output inline up to archive.inline_max_bytes; larger output uses the archive/firewall" },
                    "cwd": { "type": "string", "description": "Working dir (persists across calls)" },
                    "timeout_ms": { "type": "integer", "description": "Job lifetime in ms (max 3600000) — NOT the inline wait. A command still running at the ~110s foreground cap detaches to a pollable shell_* job and keeps running up to timeout_ms. Overridden by LEAN_CTX_SHELL_TIMEOUT_MS." },
                    "env": { "type": "object", "description": "Extra env vars", "additionalProperties": { "type": "string" } },
                    "run_in_background": { "type": "boolean", "description": "Detach immediately and return a job id. The command keeps timeout_ms; poll or cancel with background_action and job_id." },
                    "background_action": { "type": "string", "enum": ["status", "cancel"], "description": "Inspect or cancel a background ctx_shell job. During the documented 5-minute retention window, status exposes the real terminal state and exit code; bounded retention may evict older jobs under pressure, after which status returns a structured lookup error." },
                    "job_id": { "type": "string", "description": "Job id returned by run_in_background." }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        if let Some(message) = shell_access_denial(ctx) {
            return Ok(ToolOutput {
                shell_outcome: Some(ShellOutcome::Blocked),
                content_blocks: None,
                ..ToolOutput::simple(message)
            });
        }

        if let Some(action) = get_str(args, "background_action") {
            let id = get_str(args, "job_id").ok_or_else(|| {
                ErrorData::invalid_params("job_id is required with background_action", None)
            })?;
            let is_cancel = action == "cancel";
            let state = match action.as_str() {
                "status" => crate::server::background_shell::status(&id),
                "cancel" => crate::server::background_shell::cancel(&id),
                _ => {
                    return Err(ErrorData::invalid_params(
                        "background_action must be status or cancel",
                        None,
                    ));
                }
            };
            let (text, outcome) = format_background_state(&id, is_cancel, state);
            return Ok(ToolOutput {
                shell_outcome: Some(outcome),
                content_blocks: None,
                ..ToolOutput::simple(text)
            });
        }
        let command = get_str(args, "command")
            .ok_or_else(|| ErrorData::invalid_params("command is required", None))?;
        let timeout_ms = get_int(args, "timeout_ms").and_then(|n| u64::try_from(n).ok());

        // The write-doctrine check (no `>`, `tee`, heredoc-to-file, curl -o, …)
        // is an MCP-payload-safety convention, not a security boundary, so it is
        // opt-out via `shell_allow_writes` (#523). The real command gating
        // (`check_shell_allowlist`, below) is NOT affected by this flag.
        let config = crate::core::config::Config::load();
        let write_allow_paths = config.shell_write_allow_paths_effective();
        let project_root = crate::core::config::Config::find_project_root();
        if !config.shell_allow_writes_effective()
            && let Some(rejection) =
                crate::tools::ctx_shell::validate_command_with_write_allow_paths(
                    &command,
                    &write_allow_paths,
                    project_root.as_deref(),
                )
        {
            // The command never ran — report as a tool error so MCP clients
            // (guards, retry logic) can detect it programmatically (#389).
            return Ok(ToolOutput {
                shell_outcome: Some(ShellOutcome::Blocked),
                content_blocks: None,
                ..ToolOutput::simple(rejection)
            });
        }

        if let Some((lang, code, remainder)) = detect_heredoc_reroute(&command) {
            return tokio::task::block_in_place(|| {
                handle_interpreter_heredoc_reroute(args, ctx, &lang, &code, remainder)
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
            let guard = crate::server::bounded_lock::read(session_lock, "ctx_shell_cwd");
            let (effective_cwd, cwd_jail_reason) =
                resolve_effective_cwd(guard, explicit_cwd.as_deref())?;
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
            let proactive_block = if raw
                || !crate::core::profiles::active_profile()
                    .output_hints
                    .proactive_context()
            {
                None
            } else {
                crate::core::relevance_tracker::proactive_context(&format!(
                    "ctx_shell command={cmd_clone} cwd={cwd_clone}"
                ))
            };

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

            let inline = get_bool(args, "inline").unwrap_or(false);
            let shell_cache_key = shell_cache_key(&cmd_clone, &cwd_clone, &extra_env);
            if !raw
                && !inline
                && crate::core::config::Config::load()
                    .cache
                    .shell_cache_enabled
                && let Some(key) = shell_cache_key.as_ref()
                && let Some(cached) = crate::core::ocla::shell_cache_allowlist::SHELL_RESULT_CACHE
                    .get(&key.cache_key())
                    .map(|r| r.clone())
                && let Ok(value) = serde_json::from_str::<Value>(&cached)
                && let (Some(text), Some(exit_code)) = (
                    value.get("text").and_then(Value::as_str),
                    value.get("exit_code").and_then(Value::as_i64),
                )
            {
                return Ok(ToolOutput {
                    text: text.to_string(),
                    original_tokens: crate::core::tokens::count_tokens(text),
                    saved_tokens: 0,
                    mode: Some("cross-agent-cache".to_string()),
                    path: None,
                    changed: false,
                    shell_outcome: Some(ShellOutcome::Exit(exit_code as i32)),
                    content_blocks: None,
                });
            }

            // Cross-process delivery: check daemon for results from other IDE tabs
            if let Some(ref key) = shell_cache_key {
                let ck = key.cache_key();
                let validator = key.validator();
                if let Some(entry) =
                    crate::core::ocla::cache_delivery::check(&ck, &validator, "ctx_shell")
                {
                    let stub = crate::core::ocla::cache_delivery::stub(&entry, "shell command");
                    return Ok(ToolOutput {
                        text: stub,
                        original_tokens: entry.token_count as usize,
                        saved_tokens: entry.token_count as usize,
                        mode: Some("cross-agent-cache".to_string()),
                        path: None,
                        changed: false,
                        shell_outcome: None,
                        content_blocks: None,
                    });
                }
            }

            let auto_background = should_auto_background(&cmd_clone, timeout_ms);
            if get_bool(args, "run_in_background").unwrap_or(false) || auto_background {
                let job_id = crate::server::background_shell::start(
                    cmd_clone, cwd_clone, extra_env, timeout_ms,
                );
                let mode = if auto_background {
                    "auto-background"
                } else {
                    "background"
                };
                return Ok(ToolOutput {
                    shell_outcome: Some(ShellOutcome::Background(BackgroundShellOutcome {
                        state: BackgroundJobState::Running,
                        exit_code: None,
                        job_id: job_id.clone(),
                        archive_id: None,
                        archive_truncated: None,
                        captured_chars: None,
                        archived_chars: None,
                        summary: format!("{mode} job started"),
                        is_error: false,
                        display: None,
                    })),
                    content_blocks: None,
                    ..ToolOutput::simple(format!(
                        "[{mode}:{job_id} started — use ctx_shell(background_action=\"status\", job_id=\"{job_id}\") to poll or background_action=\"cancel\" to stop it]"
                    ))
                });
            }

            // Foreground runs still detach onto a pollable job if they outlast
            // the soft cap, so the MCP host's ~120s abort never strands the
            // result behind an unresolvable task id (#1106).
            //
            // #1173: `timeout_ms` is the *job's* lifetime, never the foreground
            // wait, so it must not raise this cap. Raising it bought no extra
            // inline wait — the host aborts at ~120s regardless — it only
            // suppressed our own detach, producing exactly the unresolvable
            // task id the cap exists to prevent. Separate knobs, one direction:
            // `LEAN_CTX_SHELL_FG_CAP_MS` moves the cap, `timeout_ms` does not.
            let soft_cap = std::time::Duration::from_millis(foreground_soft_cap_ms());
            let progress_sender = ctx.progress_sender.clone();
            let progress_label: String = cmd_clone.chars().take(60).collect();
            let cap_secs = soft_cap.as_secs_f64();
            let on_tick = |elapsed: std::time::Duration| {
                #[allow(clippy::unwrap_or_default)]
                if let Some(ref ps) = progress_sender
                    && let Some(sender) = ps
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                {
                    sender.send(
                        elapsed.as_secs_f64(),
                        Some(cap_secs),
                        Some(format!(
                            "ctx_shell: {}s elapsed — {progress_label}",
                            elapsed.as_secs()
                        )),
                    );
                }
            };
            let (raw_output, exit_code) =
                match crate::server::background_shell::run_foreground_or_detach(
                    cmd_clone.clone(),
                    cwd_clone.clone(),
                    extra_env.clone(),
                    timeout_ms,
                    soft_cap,
                    Some(&on_tick),
                ) {
                    crate::server::background_shell::ForegroundResult::Finished {
                        output,
                        exit_code,
                    } => (output, exit_code),
                    crate::server::background_shell::ForegroundResult::Detached { job_id } => {
                        return Ok(ToolOutput {
                            shell_outcome: Some(ShellOutcome::Background(BackgroundShellOutcome {
                                state: BackgroundJobState::Running,
                                exit_code: None,
                                job_id: job_id.clone(),
                                archive_id: None,
                                archive_truncated: None,
                                captured_chars: None,
                                archived_chars: None,
                                summary: "auto-detached job is still running".to_string(),
                                is_error: false,
                                display: None,
                            })),
                            content_blocks: None,
                            // #1173: a detach is not a failure — the command is
                            // alive and its output is recoverable, so say so
                            // rather than leaving the caller to infer a hang.
                            ..ToolOutput::simple(format!(
                                "[auto-background:{job_id} still running — passed the {}s foreground cap, not an error; output is kept and delivered by ctx_shell(background_action=\"status\", job_id=\"{job_id}\"), or background_action=\"cancel\" to stop it]",
                                soft_cap.as_secs()
                            ))
                        });
                    }
                };

            // Structured diagnostics (#499) — same hook as the CLI path.
            crate::core::diagnostics_store::record_from_shell(&cmd_clone, &raw_output, exit_code);

            let output = redact_shell_output_secrets(&raw_output);

            let (result_out, original, saved, tee_hint) = if raw || inline {
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
                let timeout_notice_only = is_timeout_notice_only(&output, exit_code);
                let tee_hint = if crate::shell::tee_policy::should_tee(
                    &cfg.tee_mode,
                    exit_code,
                    output.trim().is_empty() || timeout_notice_only,
                    crate::shell::tee_policy::output_was_elided(&output, &result),
                    original,
                    sent,
                ) {
                    crate::shell::save_tee(&cmd_clone, &output)
                        .map(|p| {
                            if matches!(cfg.tee_mode, crate::core::config::TeeMode::HighCompression)
                            {
                                let pct = crate::shell::tee_policy::savings_pct(original, sent);
                                // Recovery grammar is path-first: agents without ctx_expand
                                // can still read the saved artifact directly (#936).
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
            let final_out = if let Some(block) = proactive_block {
                format!("{final_out}{block}")
            } else {
                final_out
            };

            if !raw
                && !inline
                && crate::core::config::Config::load()
                    .cache
                    .shell_cache_enabled
                && let Some(key) = shell_cache_key
            {
                let cached = json!({ "text": final_out, "exit_code": exit_code }).to_string();
                crate::core::ocla::shell_cache_allowlist::SHELL_RESULT_CACHE
                    .insert(key.cache_key(), cached);
                // Propagate to cross-process daemon cache
                crate::core::ocla::cache_delivery::record(
                    key.cache_key(),
                    crate::core::ocla::cache_types::DeliveryKind::ShellCommand,
                    key.validator(),
                    None,
                    &final_out,
                    "ctx_shell",
                );
            }

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

fn shell_cache_key(
    command: &str,
    cwd: &str,
    env: &std::collections::HashMap<String, String>,
) -> Option<ShellCommandKey> {
    if !crate::core::ocla::shell_cache_allowlist::is_cacheable_command(command) {
        return None;
    }
    let mut env_pairs = env.iter().collect::<Vec<_>>();
    env_pairs.sort_unstable_by(|left, right| left.0.cmp(right.0));
    let mut canonical_env = String::new();
    for (name, value) in env_pairs {
        canonical_env.push_str(name);
        canonical_env.push('=');
        canonical_env.push_str(value);
        canonical_env.push('\n');
    }
    Some(ShellCommandKey {
        command_normalized: crate::core::ocla::shell_cache_allowlist::normalize_command(command),
        cwd: if std::path::Path::new(cwd).is_absolute() {
            "$PROJECT_ROOT".to_string()
        } else {
            cwd.to_string()
        },
        env_hash: blake3::hash(canonical_env.as_bytes()).to_hex().to_string(),
    })
}

/// Deny shell execution for explicitly restricted MCP clients. Missing client
/// context remains allowed so existing integrations retain their current access.
fn shell_access_denial(ctx: &ToolContext) -> Option<String> {
    if let Some(role) = ctx.client_role.as_deref()
        && (role.eq_ignore_ascii_case("untrusted") || role.eq_ignore_ascii_case("readonly"))
    {
        return Some(format!(
            "[SHELL ACCESS DENIED] ctx_shell is unavailable to MCP clients with role '{role}'."
        ));
    }

    if ctx.shell_access == Some(false) {
        return Some(
            "[SHELL ACCESS DENIED] ctx_shell requires shell_access=true in the MCP session/request context."
                .to_string(),
        );
    }

    None
}

fn resolve_effective_cwd(
    session: Option<tokio::sync::OwnedRwLockReadGuard<crate::core::session::SessionState>>,
    explicit_cwd: Option<&str>,
) -> Result<(String, Option<String>), ErrorData> {
    match session {
        Some(session) => Ok(session.effective_cwd_checked(explicit_cwd)),
        None => Err(ErrorData::internal_error(
            "session lock timeout — cannot validate working directory",
            None,
        )),
    }
}

#[allow(clippy::fn_params_excessive_bools)]
fn resolve_shell_raw_flags(
    arg_raw: bool,
    arg_bypass: bool,
    _env_disabled: bool,
    env_raw: bool,
) -> (bool, bool) {
    let bypass = arg_bypass || env_raw;
    let raw = arg_raw || bypass;
    (raw, bypass)
}

/// A timeout notice is framework metadata, not recoverable command output. Do
/// not archive it as a tee artifact: expanding it cannot recover any bytes (#995).
///
/// Keyed on what precedes the marker rather than on the notice's exact shape,
/// so enriching it (the idle-timeout wording, the still-running segment list)
/// cannot silently turn every timeout back into an archived artifact (#1173).
fn is_timeout_notice_only(output: &str, exit_code: i32) -> bool {
    exit_code == 124
        && crate::server::execute::output_before_timeout_marker(output).is_some_and(str::is_empty)
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

/// Render a `background_action` result.
///
/// #1246: a caller-requested cancel is a success, not a tool failure. The
/// process's own SIGINT exit (130) used to be reported as the tool's exit code,
/// which tripped the client's failure hook and told the agent to fix something
/// it had deliberately done. A cancel therefore never reports a non-zero exit,
/// and is idempotent: cancelling an already-cancelled, already-finished or
/// already-pruned job is equally benign. The first cancel also gets its own
/// wording so it cannot be mistaken for a status poll that did nothing.
fn format_background_state(
    id: &str,
    is_cancel: bool,
    state: Option<crate::server::background_shell::JobState>,
) -> (String, ShellOutcome) {
    use crate::server::background_shell::JobState;
    let Some(state) = state else {
        return if is_cancel {
            (
                format!("[background:{id} not found — already finished or cancelled]"),
                ShellOutcome::Exit(0),
            )
        } else {
            (
                format!("[background:{id} not found or expired]"),
                ShellOutcome::BackgroundLookupError(BackgroundLookupError {
                    job_id: id.to_string(),
                    code: "background_job_not_found_or_expired".to_string(),
                    reason: "job not found or retained terminal verdict expired".to_string(),
                }),
            )
        };
    };
    match state {
        JobState::Running { output } => {
            // #1217: show the captured-so-far output so a poll of a
            // long-running job reflects progress instead of a bare
            // "running" with no signal of whether it is advancing.
            let output = redact_shell_output_secrets(&output);
            let head = if is_cancel {
                format!(
                    "[background:{id} cancel requested — job is stopping; poll status for the final output]"
                )
            } else {
                format!("[background:{id} running]")
            };
            (
                output.clone(),
                ShellOutcome::Background(BackgroundShellOutcome {
                    state: BackgroundJobState::Running,
                    exit_code: None,
                    job_id: id.to_string(),
                    archive_id: None,
                    archive_truncated: None,
                    captured_chars: None,
                    archived_chars: None,
                    summary: summarize_background_output(&output),
                    is_error: false,
                    display: Some(BackgroundDisplay {
                        header: head,
                        footer: None,
                    }),
                }),
            )
        }
        JobState::Completed { output, exit_code } => {
            let output = redact_shell_output_secrets(&output);
            let state = if exit_code == 0 {
                BackgroundJobState::Completed
            } else {
                BackgroundJobState::Failed
            };
            let head = format!("[background:{id} {}, exit {exit_code}]", state.as_str());
            let footer = (exit_code != 0).then(|| format!("[exit:{exit_code}]"));
            (
                output.clone(),
                ShellOutcome::Background(BackgroundShellOutcome {
                    state,
                    exit_code: Some(exit_code),
                    job_id: id.to_string(),
                    archive_id: None,
                    archive_truncated: None,
                    captured_chars: None,
                    archived_chars: None,
                    summary: summarize_background_output(&output),
                    is_error: !is_cancel && exit_code != 0,
                    display: Some(BackgroundDisplay {
                        header: head,
                        footer,
                    }),
                }),
            )
        }
        JobState::Cancelled { output } => {
            let output = redact_shell_output_secrets(&output);
            (
                output.clone(),
                ShellOutcome::Background(BackgroundShellOutcome {
                    state: BackgroundJobState::Cancelled,
                    exit_code: Some(130),
                    job_id: id.to_string(),
                    archive_id: None,
                    archive_truncated: None,
                    captured_chars: None,
                    archived_chars: None,
                    summary: summarize_background_output(&output),
                    is_error: false,
                    display: Some(BackgroundDisplay {
                        header: format!("[background:{id} cancelled, exit 130]"),
                        footer: Some(format!("[cancelled: {id}, exit 130]")),
                    }),
                }),
            )
        }
    }
}

fn summarize_background_output(output: &str) -> String {
    let chars = output.chars().count();
    let lines = output.lines().count();
    let trimmed = output.trim();
    if trimmed.is_empty() {
        "no output".to_string()
    } else if chars <= 512 {
        trimmed.to_string()
    } else {
        format!("{chars} chars, {lines} lines")
    }
}

fn redact_shell_output_secrets(output: &str) -> String {
    let output = crate::core::redaction::redact_text_if_enabled(output);
    let cfg = crate::core::config::Config::load();
    if !cfg.secret_detection.enabled {
        return output;
    }
    let (redacted, matches) =
        crate::core::secret_detection::scan_and_redact(&output, &cfg.secret_detection);
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

/// The Codex MCP client abandons a tool call after five minutes. A Cargo test
/// with an explicit five-minute-or-longer shell timeout cannot reliably finish
/// inside that transport deadline (and can wait on Cargo's target lock), so
/// detach it and return a pollable job instead.
fn should_auto_background(command: &str, timeout_ms: Option<u64>) -> bool {
    timeout_ms.is_some_and(|timeout| timeout >= 300_000)
        && command
            .lines()
            .any(|line| line.trim_start().starts_with("cargo test"))
}

/// Foreground wait budget before a still-running command is detached into a
/// pollable background job. Kept below the MCP host's ~120s tool-call abort so
/// the caller always receives a real `shell_*` job id instead of an
/// unresolvable task id (#1106). Override with `LEAN_CTX_SHELL_FG_CAP_MS`.
fn foreground_soft_cap_ms() -> u64 {
    std::env::var("LEAN_CTX_SHELL_FG_CAP_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(110_000)
}

/// Map an interpreter binary to a `ctx_execute` language name.
fn interpreter_to_execute_language(base: &str) -> Option<&'static str> {
    match base {
        "python" | "python2" | "python3" => Some("python"),
        "node" => Some("javascript"),
        "ruby" => Some("ruby"),
        _ => None,
    }
}

fn is_env_assignment_token(token: &str) -> bool {
    let unquoted: String = token.chars().filter(|c| *c != '"' && *c != '\'').collect();
    unquoted.contains('=')
        && !unquoted.starts_with('-')
        && !unquoted.starts_with('/')
        && !unquoted.starts_with('.')
}

/// Quote-aware scan for compound operators that would invalidate a reroute.
fn prelude_has_compound_operator(prelude: &str) -> bool {
    let bytes = prelude.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < len {
        let ch = bytes[i];
        if in_single {
            if ch == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if ch == b'\\' && i + 1 < len {
                i += 2;
            } else {
                if ch == b'"' {
                    in_double = false;
                }
                i += 1;
            }
            continue;
        }
        match ch {
            b'\'' => {
                in_single = true;
                i += 1;
            }
            b'"' => {
                in_double = true;
                i += 1;
            }
            b';' | b'|' | b'&' | b'(' | b'{' => return true,
            _ => i += 1,
        }
    }
    false
}

/// Detect interpreter heredoc patterns and extract the language + code body.
/// Returns `Some((language, code, remainder))` where remainder is any command
/// after the heredoc terminator that still needs shell execution.
fn detect_heredoc_reroute(command: &str) -> Option<(String, String, Option<String>)> {
    if !command.contains("<<") {
        return None;
    }

    let lines: Vec<&str> = command.lines().collect();
    if lines.is_empty() {
        return None;
    }

    let first_line = lines[0];
    let delims = crate::core::shell_allowlist::heredoc_delims(first_line, false);
    if delims.len() != 1 {
        return None;
    }
    let delim = delims[0].clone();

    let heredoc_pos = find_heredoc_operator(first_line)?;
    let prelude = first_line[..heredoc_pos].trim();
    if prelude_has_compound_operator(prelude) {
        return None;
    }

    let language = parse_interpreter_heredoc_prelude(prelude)?.to_string();

    if has_trailing_tokens_after_heredoc_delim(first_line, heredoc_pos) {
        return None;
    }

    let mut body = String::new();
    let mut remainder_start: Option<usize> = None;
    for (idx, line) in lines.iter().enumerate().skip(1) {
        if line.trim_start_matches('\t').trim() == delim {
            remainder_start = Some(idx + 1);
            break;
        }
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(line);
    }
    let remainder_start = remainder_start?;
    let remainder = if remainder_start < lines.len() {
        let rest = lines[remainder_start..].join("\n");
        let rest = rest.trim();
        if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        }
    } else {
        None
    };

    Some((language, body, remainder))
}

fn find_heredoc_operator(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < len {
        let ch = bytes[i];
        if in_single {
            if ch == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if ch == b'\\' && i + 1 < len {
                i += 2;
            } else {
                if ch == b'"' {
                    in_double = false;
                }
                i += 1;
            }
            continue;
        }
        match ch {
            b'\'' => {
                in_single = true;
                i += 1;
            }
            b'"' => {
                in_double = true;
                i += 1;
            }
            b'<' if i + 1 < len && bytes[i + 1] == b'<' => {
                if i + 2 < len && bytes[i + 2] == b'<' {
                    i += 3;
                    continue;
                }
                return Some(i);
            }
            _ => i += 1,
        }
    }
    None
}

fn has_trailing_tokens_after_heredoc_delim(line: &str, heredoc_pos: usize) -> bool {
    let bytes = line.as_bytes();
    let mut i = heredoc_pos + 2;
    if i < bytes.len() && bytes[i] == b'-' {
        i += 1;
    }
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let Some((_, _, next)) = crate::core::shell_allowlist::read_heredoc_delim(bytes, i) else {
        return true;
    };
    !line[next..].trim().is_empty()
}

fn parse_interpreter_heredoc_prelude(prelude: &str) -> Option<&'static str> {
    let tokens = crate::core::shell_allowlist::shell_tokenize(prelude.trim());
    let mut idx = 0;
    while idx < tokens.len() && is_env_assignment_token(&tokens[idx]) {
        idx += 1;
    }
    if idx >= tokens.len() {
        return None;
    }
    let base = tokens[idx]
        .rsplit('/')
        .next()
        .unwrap_or(tokens[idx].as_str());
    let language = interpreter_to_execute_language(base)?;
    idx += 1;
    match tokens.get(idx) {
        None => Some(language),
        Some(dash) if dash == "-" => {
            if tokens.len() == idx + 1 {
                Some(language)
            } else {
                None
            }
        }
        Some(_) => None,
    }
}

fn handle_interpreter_heredoc_reroute(
    args: &Map<String, Value>,
    ctx: &ToolContext,
    language: &str,
    code: &str,
    remainder: Option<String>,
) -> Result<ToolOutput, ErrorData> {
    let timeout_ms = get_int(args, "timeout_ms").and_then(|n| u64::try_from(n).ok());
    let timeout_secs = timeout_ms.map(|ms| ms.div_ceil(1000).max(1));

    let (exec_text, exec_outcome) =
        crate::tools::ctx_execute::handle(language, code, None, timeout_secs);
    let reroute_note = format!(
        "\n[ctx_shell: interpreter heredoc auto-rerouted to ctx_execute(language=\"{language}\")]"
    );
    let exec_text = crate::core::redaction::redact_text_if_enabled(&exec_text);

    let Some(rest_cmd) = remainder else {
        return Ok(ToolOutput {
            text: format!("{exec_text}{reroute_note}"),
            original_tokens: crate::core::tokens::count_tokens(&exec_text),
            saved_tokens: 0,
            mode: Some("heredoc-reroute".to_string()),
            path: None,
            changed: false,
            shell_outcome: Some(exec_outcome),
            content_blocks: None,
        });
    };

    if let Err(msg) = crate::core::shell_allowlist::check_shell_allowlist(&rest_cmd) {
        let blocked =
            format!("{exec_text}{reroute_note}\n\n[remainder blocked by shell allowlist]\n{msg}");
        return Ok(ToolOutput {
            shell_outcome: Some(ShellOutcome::Blocked),
            content_blocks: None,
            ..ToolOutput::simple(blocked)
        });
    }

    let session_lock = ctx
        .session
        .as_ref()
        .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
    let explicit_cwd = get_str(args, "cwd");
    let guard = crate::server::bounded_lock::read(session_lock, "ctx_shell_cwd");
    let (effective_cwd, _) = resolve_effective_cwd(guard, explicit_cwd.as_deref())?;

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

    let (shell_raw, shell_exit) = crate::server::execute::execute_command_with_env(
        &rest_cmd,
        &effective_cwd,
        &extra_env,
        timeout_ms,
    );
    let shell_output = redact_shell_output_secrets(&shell_raw);
    let arg_raw = get_bool(args, "raw").unwrap_or(false);
    let arg_bypass = get_bool(args, "bypass").unwrap_or(false);
    let env_disabled = std::env::var("LEAN_CTX_DISABLED").is_ok();
    let env_raw = std::env::var("LEAN_CTX_RAW").is_ok();
    let (raw, _) = resolve_shell_raw_flags(arg_raw, arg_bypass, env_disabled, env_raw);

    let shell_text = if raw {
        shell_output
    } else {
        crate::tools::ctx_shell::handle(&rest_cmd, &shell_output, shell_exit, ctx.crp_mode)
    };
    let shell_text = crate::core::redaction::redact_text_if_enabled(&shell_text);
    let exit_suffix = match shell_exit {
        0 => String::new(),
        124 => "\n[exit:124 — command timed out]".to_string(),
        _ => format!("\n[exit:{shell_exit}]"),
    };

    let combined = format!(
        "{exec_text}{reroute_note}\n\n[heredoc remainder via ctx_shell]\n{shell_text}{exit_suffix}"
    );
    let token_count = crate::core::tokens::count_tokens(&combined);
    Ok(ToolOutput {
        text: combined,
        original_tokens: token_count,
        saved_tokens: 0,
        mode: Some("heredoc-reroute".to_string()),
        path: None,
        changed: false,
        shell_outcome: Some(ShellOutcome::Exit(shell_exit)),
        content_blocks: None,
    })
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

#[cfg(test)]
mod tests {
    use super::{
        CtxShellTool, detect_heredoc_reroute, format_background_state, is_timeout_notice_only,
        resolve_effective_cwd, shell_access_denial, should_auto_background,
    };
    use crate::server::background_shell::JobState;
    use crate::server::tool_trait::{
        BackgroundJobState, BackgroundShellOutcome, McpTool, ShellOutcome, ToolContext,
    };

    fn background(outcome: ShellOutcome) -> BackgroundShellOutcome {
        let ShellOutcome::Background(outcome) = outcome else {
            panic!("expected a background outcome");
        };
        outcome
    }

    /// #1246: a cancel must never come back as a tool error, and must not read
    /// like a status poll that did nothing.
    #[test]
    fn cancel_is_acknowledged_and_never_reports_a_failure() {
        let running = JobState::Running {
            output: String::new(),
        };
        let (text, outcome) = format_background_state("shell_x", true, Some(running.clone()));
        let outcome = background(outcome);
        assert!(!outcome.is_error);
        assert_eq!(outcome.state, BackgroundJobState::Running);
        assert_eq!(outcome.exit_code, None);
        assert!(text.is_empty());
        assert!(
            outcome
                .display
                .as_ref()
                .is_some_and(|display| display.header.contains("cancel requested"))
        );

        // A status poll of the same state keeps the old wording.
        let (text, outcome) = format_background_state("shell_x", false, Some(running));
        let outcome = background(outcome);
        assert!(!outcome.is_error);
        assert_eq!(outcome.state, BackgroundJobState::Running);
        assert!(text.is_empty());
        assert!(
            outcome
                .display
                .as_ref()
                .is_some_and(|display| display.header == "[background:shell_x running]")
        );

        // The terminal child code is data; the requested cancel remains a
        // successful tool action even though structuredContent keeps 130.
        let (text, outcome) = format_background_state(
            "shell_x",
            true,
            Some(JobState::Cancelled {
                output: "[cancelled: command stopped on request]".to_string(),
            }),
        );
        let outcome = background(outcome);
        assert!(!outcome.is_error);
        assert_eq!(outcome.state, BackgroundJobState::Cancelled);
        assert_eq!(outcome.exit_code, Some(130));
        assert!(text.contains("[cancelled: command stopped on request]"));
        assert!(
            outcome
                .display
                .as_ref()
                .and_then(|display| display.footer.as_deref())
                .is_some_and(|footer| footer == "[cancelled: shell_x, exit 130]")
        );

        // Idempotent: already finished, or finished and pruned.
        let finished = JobState::Completed {
            output: "boom".to_string(),
            exit_code: 1,
        };
        let cancelled_finished =
            background(format_background_state("shell_x", true, Some(finished.clone())).1);
        assert!(!cancelled_finished.is_error);
        assert_eq!(cancelled_finished.state, BackgroundJobState::Failed);
        assert_eq!(cancelled_finished.exit_code, Some(1));

        let polled_finished =
            background(format_background_state("shell_x", false, Some(finished)).1);
        assert!(polled_finished.is_error);
        assert_eq!(polled_finished.state, BackgroundJobState::Failed);
        assert_eq!(polled_finished.exit_code, Some(1));

        let missing_cancel = format_background_state("shell_x", true, None).1;
        assert!(!missing_cancel.is_error());
        assert_eq!(missing_cancel, ShellOutcome::Exit(0));

        let missing_status = format_background_state("shell_x", false, None).1;
        assert!(missing_status.is_error());
        assert!(matches!(
            missing_status,
            ShellOutcome::BackgroundLookupError(_)
        ));
    }

    #[test]
    fn long_cargo_test_is_auto_backgrounded() {
        assert!(should_auto_background(
            "cargo test --lib a\ncargo test --lib b",
            Some(3_600_000)
        ));
        assert!(should_auto_background("cargo test --lib a", Some(300_000)));
        assert!(!should_auto_background("cargo test --lib a", Some(299_999)));
    }

    #[test]
    fn timeout_notice_without_child_output_is_not_recoverable() {
        assert!(is_timeout_notice_only(
            "ERROR: command timed out after 200ms",
            124
        ));
        assert!(is_timeout_notice_only(
            "  ERROR: command timed out after 200ms\n",
            124
        ));
        assert!(!is_timeout_notice_only(
            "useful output\nERROR: command timed out after 200ms",
            124
        ));
        assert!(!is_timeout_notice_only(
            "ERROR: command timed out after 200ms",
            1
        ));
        // #1173: the notice now carries the idle wording and the still-running
        // segment list. It is still pure metadata — nothing to recover — so it
        // must not become a tee artifact just because it grew.
        assert!(is_timeout_notice_only(
            "ERROR: command timed out after 200ms without new output\n\
             [still running at timeout: sleep 300]",
            124
        ));
        assert!(!is_timeout_notice_only(
            "useful output\nERROR: command timed out after 200ms\n\
             [still running at timeout: sleep 300]",
            124
        ));
        // Exit 124 from something that is not our watchdog carries no marker.
        assert!(!is_timeout_notice_only("some tool output", 124));
    }

    #[test]
    fn unavailable_session_lock_rejects_explicit_cwd() {
        let error = resolve_effective_cwd(None, Some("/tmp/unvalidated"))
            .expect_err("an unavailable session lock must reject an unvalidated cwd");
        assert!(
            error.message.contains("cannot validate working directory"),
            "{error:?}"
        );
    }

    #[test]
    fn untrusted_and_readonly_clients_cannot_run_shell_commands() {
        for role in ["untrusted", "readonly"] {
            let ctx = ToolContext {
                client_role: Some(role.to_string()),
                ..ToolContext::default()
            };
            let output = CtxShellTool
                .handle(&serde_json::Map::new(), &ctx)
                .expect("role denial must be a tool result");

            assert_eq!(output.shell_outcome, Some(ShellOutcome::Blocked));
            assert!(
                output.text.contains("SHELL ACCESS DENIED"),
                "{role}: {}",
                output.text
            );
            assert!(output.text.contains(role), "{role}: {}", output.text);
        }
    }

    #[test]
    fn absent_shell_context_preserves_default_access() {
        assert!(shell_access_denial(&ToolContext::default()).is_none());
    }

    #[test]
    fn explicitly_disabled_shell_access_blocks_the_request() {
        let ctx = ToolContext {
            shell_access: Some(false),
            ..ToolContext::default()
        };
        let output = CtxShellTool
            .handle(&serde_json::Map::new(), &ctx)
            .expect("shell-access denial must be a tool result");

        assert_eq!(output.shell_outcome, Some(ShellOutcome::Blocked));
        assert!(output.text.contains("shell_access=true"), "{}", output.text);
    }

    #[test]
    fn detect_heredoc_reroute_python_quoted() {
        let cmd = "python3 - <<'PY'\nprint(1)\nPY";
        let (lang, code, rest) = detect_heredoc_reroute(cmd).expect("must detect python heredoc");
        assert_eq!(lang, "python");
        assert_eq!(code, "print(1)");
        assert!(rest.is_none());
    }

    #[test]
    fn detect_heredoc_reroute_python_with_remainder() {
        let cmd = "python3 <<'PY'\nprint(1)\nPY\nnode --test file.js";
        let (lang, code, rest) = detect_heredoc_reroute(cmd).expect("must detect split heredoc");
        assert_eq!(lang, "python");
        assert_eq!(code, "print(1)");
        assert_eq!(rest.as_deref(), Some("node --test file.js"));
    }

    #[test]
    fn detect_heredoc_reroute_unquoted_and_tab_stripped() {
        let unquoted = "ruby <<EOF\nputs 1\nEOF";
        let (lang, code, rest) = detect_heredoc_reroute(unquoted).unwrap();
        assert_eq!(lang, "ruby");
        assert_eq!(code, "puts 1");
        assert!(rest.is_none());

        let tabbed = "python3 <<-\tSCRIPT\n\tprint('ok')\nSCRIPT";
        let (lang, code, rest) = detect_heredoc_reroute(tabbed).unwrap();
        assert_eq!(lang, "python");
        assert_eq!(code, "\tprint('ok')");
        assert!(rest.is_none());
    }

    #[test]
    fn detect_heredoc_reroute_rejects_compound_prefix() {
        assert!(detect_heredoc_reroute("echo hi; python3 <<'PY'\nx\nPY").is_none());
        assert!(detect_heredoc_reroute("python3 -c 'print(1)'").is_none());
        assert!(detect_heredoc_reroute("python3 <<'PY' | cat\nx\nPY").is_none());
    }
}
