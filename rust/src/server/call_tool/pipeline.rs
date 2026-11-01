// SPDX-License-Identifier: Apache-2.0

#[allow(clippy::wildcard_imports)]
use super::super::*;
use super::{append_rules_self_heal_status, finalize_call_result};

pub(in crate::server) enum McpPrimitive {
    Raw(McpRawOutput),
    Terminal(CallToolResult, &'static str),
}

impl McpPrimitive {
    /// A tool failure as telemetry sees it (class and scrubbed template),
    /// derived locally from its message; `None` when the tool succeeded.
    pub(in crate::server) fn failure(&self) -> Option<crate::core::telemetry_failure::Failure> {
        match self {
            Self::Raw(raw) => raw
                .tool_error
                .then(|| crate::core::telemetry_failure::Failure::from_message(&raw.result_text)),
            Self::Terminal(result, reason) => result_failure(result, reason),
        }
    }
}

pub(in crate::server) struct McpRawOutput {
    archive_authority: Option<Box<crate::core::archive::authority::ArchiveAuthority>>,
    result_text: String,
    tool_error: bool,
    tool_saved_tokens: usize,
    shell_outcome: Option<crate::server::tool_trait::ShellOutcome>,
    content_blocks: Option<Vec<ContentBlock>>,
    tool_start: std::time::Instant,
}

pub(in crate::server) struct McpProcessed {
    pub result: CallToolResult,
    ir: Option<McpIrIntent>,
    ledger: Option<McpLedgerIntent>,
    receipt: Option<McpReceiptIntent>,
    pub checkpoint: Option<McpCheckpointIntent>,
}

pub(in crate::server) struct McpCheckpointIntent {
    enabled: bool,
    output_visible: bool,
}

impl McpProcessed {
    pub(in crate::server) fn freeze_receipt_delivery(&mut self) {
        if let Some(checkpoint) = self.checkpoint.as_mut() {
            // Checkpoint state still executes; only later prose is suppressed.
            checkpoint.output_visible = false;
        }
    }
}

struct McpIrIntent {
    kind: crate::core::context_ir::ContextIrSourceKindV1,
    tool: String,
    path: Option<String>,
    command: Option<String>,
    pattern: Option<String>,
    input_tokens: usize,
    output_tokens: usize,
    duration: std::time::Duration,
    content_excerpt: String,
}

struct McpLedgerIntent {
    read_path: String,
    mode_used: String,
    output_tokens: usize,
    sent_tokens: usize,
    project_root: Option<String>,
    wants_elicitation: bool,
}

struct McpReceiptIntent {
    name: String,
    args: Option<serde_json::Map<String, serde_json::Value>>,
    action: Option<String>,
    result_text: String,
    output_token_count: usize,
}

/// What the semantic tool of this call declares about its source acquisition.
/// A declaration belongs to the tool that runs, so a `ctx_call` wrapper is
/// unwrapped one level, as admission does. No registry, an unknown tool or an
/// unresolvable wrapper declares nothing.
pub(super) fn tool_declaration(
    server: &LeanCtxServer,
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> super::guarded::replay_lineage::Declaration {
    crate::tools::registered::ctx_call::resolve(name, args)
        .ok()
        .and_then(|(inner, inner_args)| {
            let tool = server.registry.as_ref()?.get_arc(&inner)?;
            Some(super::guarded::replay_lineage::Declaration {
                publishes_every_source: tool.publishes_every_source(inner_args.as_ref()),
                attests_source_free: tool.attests_source_free(inner_args.as_ref()),
            })
        })
        .unwrap_or_default()
}

pub(in crate::server) async fn dispatch_primitive(
    server: &LeanCtxServer,
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
    minimal: bool,
    args_fp: &str,
) -> Result<McpPrimitive, ErrorData> {
    let tool_start = std::time::Instant::now();
    // Ledger and security events recorded during this call commit the
    // session id into their hashes, so `lean-ctx value` can scope to it.
    crate::core::value::set_current_session(&server.session.read().await.id);
    // Before dispatch: a `cd` in the command moves the session's shell cwd,
    // but the command's relative paths are relative to where it started.
    let shell_base = if name == "ctx_shell" {
        shell_command_base_dir(server, args).await
    } else {
        None
    };
    let (dispatched, archive_authority) =
        crate::core::archive::authority::capture(server.dispatch_tool(name, args, minimal)).await;
    // What this dispatch acquired, for later reuse of its output (idempotent
    // replay, response cache): the captured source set when the tool declares
    // it complete, the tool's attestation that this invocation reads no
    // source, or — when neither holds — unknown.
    super::guarded::replay_lineage::record(
        archive_authority.as_ref(),
        tool_declaration(server, name, args),
    );
    if name == "ctx_shell"
        && let Ok((_, _, outcome, _)) = &dispatched
        && !matches!(
            outcome,
            Some(crate::server::tool_trait::ShellOutcome::Blocked)
        )
    {
        let cmd = helpers::get_str(args, "command").unwrap_or_default();
        record_shell_file_effects(&cmd, shell_base.as_deref());
    }
    let (result_text, tool_saved_tokens, shell_outcome, content_blocks) = match dispatched {
        Ok(tuple) => tuple,
        Err(e) if e.code == crate::server::tool_trait::TOOL_EXECUTION_ERROR => {
            // Retain duplicate-call accounting and the complete sensitivity,
            // policy, archive and token pipeline for failed edit excerpts.
            return Ok(McpPrimitive::Raw(McpRawOutput {
                archive_authority: None,
                result_text: e.message.into_owned(),
                tool_error: true,
                tool_saved_tokens: 0,
                shell_outcome: None,
                content_blocks: None,
                tool_start,
            }));
        }
        Err(e) => {
            if let Ok(mut detector) = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                server.loop_detector.write(),
            )
            .await
            {
                let semantic = (name == "ctx_call")
                    .then(|| crate::tools::registered::ctx_call::resolve_inner(args).ok())
                    .flatten();
                if let Some((inner_name, inner_args)) = semantic {
                    let fingerprint = inner_args.map_or_else(String::new, |args| {
                        crate::core::loop_detection::LoopDetector::fingerprint(
                            &serde_json::Value::Object(args),
                        )
                    });
                    detector.record_error_outcome(&inner_name, &fingerprint);
                } else {
                    detector.record_error_outcome(name, &args_fp);
                }
            }
            crate::core::debug_log::log_mcp_error(name, args, &format!("{e:?}"));

            // Devin/Windsurf treat hard -32602 as transport failure and
            // respawn the server. Return a soft tool error so the agent
            // sees the validation message and can fix parameter names.
            if e.code == rmcp::model::ErrorCode::INVALID_PARAMS {
                record_error_category(mcp_error_category(e.code));
                if crate::core::policy::runtime::is_active() {
                    tracing::debug!("converting protected INVALID_PARAMS to soft tool error");
                } else {
                    tracing::debug!(
                        "converting INVALID_PARAMS to soft tool error for '{name}': {}",
                        e.message
                    );
                }
                let result = CallToolResult::error(vec![ContentBlock::text(e.message.to_string())]);
                return Ok(McpPrimitive::Terminal(result, "invalid params"));
            }

            record_error_category(mcp_error_category(e.code));
            return Err(e);
        }
    };
    Ok(McpPrimitive::Raw(McpRawOutput {
        archive_authority: archive_authority.map(Box::new),
        result_text,
        tool_error: false,
        tool_saved_tokens,
        shell_outcome,
        content_blocks,
        tool_start,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::server) async fn reversible_post_process(
    server: &LeanCtxServer,
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
    minimal: bool,
    config: std::sync::Arc<crate::core::config::Config>,
    machine_readable: bool,
    auto_context: Option<String>,
    throttle_warning: Option<String>,
    mut decision_context: Option<crate::core::decision_loop_runtime::TaskContext>,
    primitive: McpPrimitive,
) -> Result<McpProcessed, ErrorData> {
    let raw = match primitive {
        McpPrimitive::Raw(raw) => raw,
        McpPrimitive::Terminal(result, _) => {
            return Ok(McpProcessed {
                result: protect_terminal_result(name, result),
                ir: None,
                ledger: None,
                receipt: None,
                checkpoint: None,
            });
        }
    };
    let McpRawOutput {
        archive_authority,
        mut result_text,
        tool_error,
        tool_saved_tokens,
        shell_outcome,
        content_blocks,
        tool_start,
    } = raw;
    // Classified from the tool's own message, before any decoration, so the
    // category agrees with the failure class `McpPrimitive::failure` reports.
    let tool_error_category = tool_error.then(|| {
        tool_execution_error_category(crate::core::telemetry_failure::classify(&result_text))
    });
    let mut shell_outcome = shell_outcome;
    // Admission and dispatch retain the outer wrapper's gates. Delivery belongs
    // to the resolved leaf tool, including raw flags and source-bound recovery.
    let delivery_call = crate::tools::registered::ctx_call::resolve(name, args).ok();
    let (delivery_name, delivery_args) = delivery_call
        .as_ref()
        .map_or((name, args), |(name, args)| (name.as_str(), args.as_ref()));

    // Inspect before triage, IR excerpts, archives or any output decoration.
    // Raw/full/compression bypasses never bypass a content policy.
    if let Some(blocks) = content_blocks {
        if crate::core::policy::runtime::is_active() {
            let Some(texts) = blocks
                .iter()
                .map(|block| block.as_text().map(|text| text.text.as_str()))
                .collect::<Option<Vec<_>>>()
            else {
                return Ok(policy_blocked_output(
                    "[POLICY BLOCKED] Non-text content cannot be inspected by the active policy.",
                ));
            };
            result_text = texts.join("\n");
        } else {
            let mut result = CallToolResult::success(blocks);
            if shell_outcome
                .as_ref()
                .is_some_and(crate::server::tool_trait::ShellOutcome::is_error)
            {
                result.is_error = Some(true);
            }
            return Ok(McpProcessed {
                result,
                ir: None,
                ledger: None,
                receipt: None,
                checkpoint: None,
            });
        }
    }
    let knowledge_error = name == "ctx_knowledge" && result_text.starts_with("Error:");
    let diagnostic_fields =
        crate::core::policy::diagnostics::fields(&[("result", &result_text)], args)
            .map(std::sync::Arc::new);
    result_text = match policy_guard::protect_result(name, &result_text) {
        Ok(text) => text,
        Err(reason) => return Ok(policy_blocked_output(reason)),
    };
    if knowledge_error {
        return Ok(McpProcessed {
            result: CallToolResult::error(vec![ContentBlock::text(result_text)]),
            ir: None,
            ledger: None,
            receipt: None,
            checkpoint: None,
        });
    }
    let path_hint = helpers::get_str(args, "path");
    result_text = crate::core::sensitivity::enforce_text(
        result_text,
        path_hint.as_deref().map(std::path::Path::new),
        &config.sensitivity_effective(),
    )
    .into_text();

    let task_profile = {
        let session = server.session.read().await;
        crate::core::decision_loop_runtime::DecisionLoopRuntime::get_or_init()
            .profile_for_session(&session.id)
    };
    let background_status = shell_outcome
        .as_ref()
        .is_some_and(crate::server::tool_trait::ShellOutcome::is_background_status);
    let terminal_background_status = shell_outcome
        .as_ref()
        .is_some_and(crate::server::tool_trait::ShellOutcome::is_terminal_background_status);
    // #1484: respect lossless escape hatches — never triage when the caller
    // explicitly requested unfiltered output (raw, aggressiveness=0, fresh=true).
    // #1490: an explicit lines:N-M / anchored:N-M window is already an
    // agent-chosen minimal selection — triage has nothing useful to strip.
    // #1492: mode="full" is documented as "verbatim, edit-ready" — triaging it
    // defeats its contract and causes agents to edit against incomplete content.
    let triage_bypass = triage_bypass_requested(delivery_name, delivery_args)
        || crate::tools::ctx_provider::is_snapshot_call(name, args);
    // Background-status verdicts are archived verbatim first; their display
    // variant is triaged after the archive/firewall step below (#1510), so
    // the structured lifecycle text the archive keeps stays complete.
    if !background_status && !triage_bypass {
        result_text = apply_task_triage_filter(
            result_text,
            task_profile.as_ref(),
            &mut decision_context,
            config.decision_loop.max_filter_level.min(2),
        );
    }

    // #1484 (bug 3): if triage filtered lines, invalidate the dedup cache for
    // the tool path so subsequent reads deliver full content instead of a stub.
    if let Some(ref ctx) = decision_context {
        if ctx.filtered_lines > 0 {
            if let Some(path) = args.and_then(|a| a.get("path").and_then(|p| p.as_str())) {
                crate::tools::ctx_read::dedup_hook::on_write(path);
            }
        }
    }

    let inline_shell = delivery_name == "ctx_shell"
        && crate::core::firewall::should_inline_shell(
            helpers::get_bool(delivery_args, "inline").unwrap_or(false),
            result_text.len(),
            &config,
        );
    // Explicit verbatim (raw/bypass): the caller took responsibility for the
    // exact bytes — never digested, never capped (#1453, #1541).
    let explicit_verbatim_shell = delivery_name == "ctx_shell" && {
        let arg_raw = helpers::get_bool(delivery_args, "raw").unwrap_or(false);
        let arg_bypass = helpers::get_bool(delivery_args, "bypass").unwrap_or(false);
        arg_raw || arg_bypass || crate::core::runtime_flags::raw_enabled()
    };
    let is_raw_shell = explicit_verbatim_shell
        || (delivery_name == "ctx_shell"
            && (inline_shell
                // #1260: dataset output (sqlite3/psql/jq/`gh --json`) is destroyed,
                // not compressed, by middle elision — pass it through at any size
                // up to the context-window cap (#1541).
                || helpers::get_str(delivery_args, "command")
                    .is_some_and(|c| crate::core::firewall::is_raw_command(&c, &config))));

    let pre_terse_len = result_text.len();
    let output_tokens = crate::core::tokens::count_tokens(&result_text) as u64;

    // Capture Context IR now; the lifecycle writes it after post-processing.
    let ir_intent = server.context_ir.as_ref().map(|_| {
        let source_kind = post_process::context_ir_source_kind(name);
        let protected_arg = |key| {
            helpers::get_str(args, key)
                .and_then(|value| policy_guard::protect_result(name, &value).ok())
        };
        let ir_path = protected_arg("path");
        let ir_command = protected_arg("command");
        let ir_mode = protected_arg("mode");
        let excerpt = if result_text.len() > 200 {
            let mut end = 200;
            while !result_text.is_char_boundary(end) && end > 0 {
                end -= 1;
            }
            &result_text[..end]
        } else {
            &result_text
        };
        McpIrIntent {
            kind: source_kind,
            tool: name.to_owned(),
            path: ir_path,
            command: ir_command,
            pattern: ir_mode,
            input_tokens: pre_terse_len / 4,
            output_tokens: output_tokens as usize,
            duration: tool_start.elapsed(),
            content_excerpt: excerpt.to_owned(),
        }
    });

    // Correction-loop detection: track re-reads and re-runs as quality signals.
    {
        let mut detector = server.loop_detector.write().await;
        if name == "ctx_read" {
            let path = helpers::get_str(args, "path").unwrap_or_default();
            let mode = helpers::get_str(args, "mode").unwrap_or_else(|| "auto".into());
            let fresh = helpers::get_bool(args, "fresh").unwrap_or(false);
            detector.record_read_for_correction(&path, &mode, fresh);
        } else if name == "ctx_shell" {
            let cmd = helpers::get_str(args, "command").unwrap_or_default();
            detector.record_shell_for_correction(&cmd);
        } else if name == "ctx_expand" {
            // CCR-learning (#941): a verbatim/original re-fetch means the inline
            // compressed form was too lossy for this session.
            detector.record_retrieve();
        }
        let correction_count = detector.correction_count();
        let retrieve_count = detector.retrieve_count();
        // Auto-degrade: reduce compression when the agent keeps re-fetching what
        // we squeezed out. Correction loops (re-reads/re-runs) and CCR retrieves
        // (ctx_expand/ctx_retrieve) are two views of the same "too aggressive"
        // signal; degrade on the stronger of the two and clear only when neither
        // fires. The level is server state, never part of any output body (#498).
        use crate::core::config::CompressionLevel;
        CompressionLevel::apply_degrade_action(CompressionLevel::degrade_action(
            correction_count,
            retrieve_count,
        ));
        detector.prune_corrections();
    }

    // Out-of-band archive + optional context firewall for large tool outputs.
    // For firewallable tools (ctx_shell/ctx_execute/ctx_search/ctx_tree) whose output
    // exceeds the ephemeral threshold, the full (redacted) body is stored out-of-band
    // and the inline result is replaced by a compact digest + ctx_expand drilldown.
    let shell_notice = crate::server::execute::leading_notice(&result_text);
    let mut firewalled = false;
    let mut firewall_saved_tokens: usize = 0;
    // GH #1453: `raw`/`inline`/`bypass` explicitly request verbatim delivery —
    // the firewall must NOT replace their content with a structural digest.
    // The output is still *archived* (so ctx_expand works), but stays inline.
    // #1540: only the LEAN_CTX_MINIMAL env escape hatch skips archiving — the
    // `minimal_overhead` config key (default true) trims instruction overhead
    // and must never disable the archive/firewall safety net.
    let archive_hint = if crate::core::config::Config::minimal_escape_hatch() {
        None
    } else {
        let name = delivery_name;
        let args = delivery_args;
        if terminal_background_status {
            use crate::core::archive;
            let chars = result_text.chars().count();
            let lines = result_text.lines().count();
            let trimmed = result_text.trim();
            let mut summary = if trimmed.is_empty() {
                "no output".to_string()
            } else if chars <= 512 {
                trimmed.to_string()
            } else {
                format!("{chars} chars, {lines} lines")
            };
            let mut stored_result = None;
            if !trimmed.is_empty() {
                let job_id = match shell_outcome.as_ref() {
                    Some(crate::server::tool_trait::ShellOutcome::Background(outcome)) => {
                        outcome.job_id.clone()
                    }
                    _ => String::new(),
                };
                let session_id = server.session.read().await.id.clone();
                let to_store = crate::core::redaction::redact_text_if_enabled(&result_text);
                if let Some(stored) =
                    archive::store_background(name, &job_id, &to_store, Some(&session_id))
                {
                    summary = if stored.truncated {
                        format!(
                            "{} captured chars, {} archived chars (archive truncated)",
                            stored.captured_chars, stored.archived_chars
                        )
                    } else {
                        format!("{chars} chars, {lines} lines archived")
                    };
                    if !explicit_verbatim_shell {
                        let archived = if stored.truncated {
                            archive::retrieve(&stored.id).unwrap_or_default()
                        } else {
                            to_store.clone()
                        };
                        let tokens = crate::core::tokens::count_tokens(&archived);
                        // #1541: implicit verbatim (inline/dataset) is judged
                        // against the context-window cap instead of the ephemeral
                        // threshold.
                        let fires = if is_raw_shell {
                            crate::core::firewall::verbatim_cap_exceeded(name, tokens, &config)
                        } else {
                            crate::core::firewall::should_firewall(name, tokens, &config)
                        };
                        if fires {
                            let digest = crate::core::firewall::summarize(
                                &archived, &stored.id, name, tokens, &job_id,
                            );
                            result_text = if stored.truncated {
                                format!(
                                    "[archive truncated: {} captured chars, {} archived chars; remainder unavailable]\n{digest}",
                                    stored.captured_chars, stored.archived_chars
                                )
                            } else {
                                digest
                            };
                            firewalled = true;
                            firewall_saved_tokens = tokens
                                .saturating_sub(crate::core::tokens::count_tokens(&result_text));
                        }
                    }
                    stored_result = Some(stored);
                } else {
                    summary = format!(
                        "{chars} captured chars, {lines} lines; output archive unavailable"
                    );
                    let tokens = crate::core::tokens::count_tokens(&to_store);
                    let fires = !explicit_verbatim_shell
                        && if is_raw_shell {
                            crate::core::firewall::verbatim_cap_exceeded(name, tokens, &config)
                        } else {
                            crate::core::firewall::should_firewall(name, tokens, &config)
                        };
                    if fires {
                        result_text =
                            crate::core::firewall::summarize_unavailable(&to_store, name, tokens);
                        firewalled = true;
                        firewall_saved_tokens =
                            tokens.saturating_sub(crate::core::tokens::count_tokens(&result_text));
                    }
                }
            }
            if let Some(crate::server::tool_trait::ShellOutcome::Background(outcome)) =
                shell_outcome.as_mut()
            {
                outcome.summary = summary;
                if let Some(stored) = stored_result {
                    outcome.archive_id = Some(stored.id);
                    outcome.archive_truncated = Some(stored.truncated);
                    outcome.captured_chars = Some(stored.captured_chars);
                    outcome.archived_chars = Some(stored.archived_chars);
                } else {
                    outcome.archive_id = None;
                    outcome.archive_truncated = None;
                    outcome.captured_chars = Some(chars);
                    outcome.archived_chars = None;
                }
            }
            None
        } else if background_status {
            None
        } else {
            use crate::core::archive;
            let archivable = matches!(
                name,
                "ctx_shell" | "ctx_read" | "ctx_execute" | "ctx_search" | "ctx_tree"
            );
            if archivable && archive::should_archive(&result_text) {
                let cmd = helpers::get_str(args, "command")
                    .or_else(|| helpers::get_str(args, "path"))
                    .unwrap_or_default();
                let session_id = server.session.read().await.id.clone();
                let to_store = crate::core::redaction::redact_text_if_enabled(&result_text);
                let tokens = crate::core::tokens::count_tokens(&to_store);
                // A captured source set binds the archive copy only when it is
                // the WHOLE set: the admitted file handler, and every tool
                // that declares `publishes_every_source` for this invocation.
                // Other output classes must not borrow a binding (G02: under
                // protection a declared tool's permitted output keeps a
                // source-bound, revocable recovery copy instead of none).
                let authority = ((name == "ctx_execute"
                    && helpers::get_str(args, "action").as_deref() == Some("file"))
                    || tool_declaration(server, name, args).publishes_every_source)
                    .then_some(archive_authority.as_deref())
                    .flatten()
                    // An incomplete set binds nothing. Under protection the
                    // store then refuses the copy (no source authority);
                    // without protection the output is archived originless,
                    // exactly as before source capture existed.
                    .filter(|authority| !authority.is_incomplete());
                match archive::store_with_authority(
                    name,
                    &cmd,
                    &to_store,
                    Some(&session_id),
                    authority,
                )
                .map(|stored| stored.id)
                {
                    Some(id)
                        if !is_raw_shell
                            && crate::core::firewall::should_firewall(name, tokens, &config) =>
                    {
                        result_text =
                            crate::core::firewall::summarize(&to_store, &id, name, tokens, &cmd);
                        firewalled = true;
                        firewall_saved_tokens =
                            tokens.saturating_sub(crate::core::tokens::count_tokens(&result_text));
                        None
                    }
                    // #1541: implicitly verbatim output (dataset passthrough,
                    // inline=true) keeps its row integrity up to the context-window
                    // cap; above it a single delivery floods the caller's context,
                    // so it becomes the same lossless digest + ctx_expand ref.
                    // Explicit raw/bypass is never capped.
                    Some(id)
                        if is_raw_shell
                            && !explicit_verbatim_shell
                            && crate::core::firewall::verbatim_cap_exceeded(
                                name, tokens, &config,
                            ) =>
                    {
                        result_text =
                            crate::core::firewall::summarize(&to_store, &id, name, tokens, &cmd);
                        firewalled = true;
                        firewall_saved_tokens =
                            tokens.saturating_sub(crate::core::tokens::count_tokens(&result_text));
                        None
                    }
                    Some(id) => Some(archive::format_hint(&id, to_store.len(), tokens)),
                    None => {
                        let fires = !explicit_verbatim_shell
                            && if is_raw_shell {
                                crate::core::firewall::verbatim_cap_exceeded(name, tokens, &config)
                            } else {
                                crate::core::firewall::should_firewall(name, tokens, &config)
                            };
                        if fires {
                            result_text = crate::core::firewall::summarize_unavailable(
                                &to_store, name, tokens,
                            );
                            firewalled = true;
                            firewall_saved_tokens = tokens
                                .saturating_sub(crate::core::tokens::count_tokens(&result_text));
                        }
                        None
                    }
                }
            } else {
                None
            }
        }
    };
    if firewalled {
        result_text = crate::server::execute::keep_notice(shell_notice, result_text);
        // K10: served truncated, by output class (never the command).
        use crate::core::hive::metrics_v2::signals;
        signals::truncated(&signals::output_filter(name));
    }

    if background_status && !triage_bypass && !is_raw_shell && !firewalled {
        result_text = apply_task_triage_filter(
            result_text,
            task_profile.as_ref(),
            &mut decision_context,
            config.decision_loop.max_filter_level.min(2),
        );
    }

    // #1542: the dispatcher already recorded this call's event/metering with
    // pre-firewall numbers (saved=0). Credit the firewall reduction as its own
    // zero-original correction so the live view, metering.jsonl and the
    // savings ledger reflect what the model actually receives — lossless,
    // since the full body is in the archive. Persistent stats are corrected
    // by `finalize_token_count_and_adjust` below.
    if firewalled && firewall_saved_tokens > 0 {
        crate::core::events::emit_tool_call(
            name,
            0,
            firewall_saved_tokens as u64,
            Some("firewall".to_string()),
            0,
            None,
        );
        crate::core::metering::MeterStore::append_best_effort(
            crate::core::metering::MeterEntry::new(name, 0, 0, firewall_saved_tokens as u64),
        );
        let digest_tokens = crate::core::tokens::count_tokens(&result_text) as u64;
        crate::core::savings_tracker::record_compression(
            digest_tokens.saturating_add(firewall_saved_tokens as u64),
            digest_tokens,
            name,
        );
    }

    let pre_compression = result_text.clone();
    // A firewalled result is already a compact digest — re-compressing it would mangle
    // the retrieval instructions for no benefit.
    if !firewalled {
        result_text = post_process::compress_terse(result_text, name, args, &config, is_raw_shell);
    }
    // Snapshot BEFORE any decoration (auto-context prefix, throttle/budget
    // warnings, hints): auto-findings must parse the clean tool output, or
    // the injected "--- AUTO CONTEXT ---" header itself becomes a junk
    // finding ("Read ---") that pollutes the session, the knowledge store,
    // and every subsequent wakeup briefing (#658).
    let findings_source = result_text.clone();

    // Behavior nudge (turn economy / heavy reads, #848 follow-up): observe
    // every call so chain detection stays complete; append the budgeted
    // one-line hint only to plain, non-firewalled results.
    if let Some(hint) = crate::core::behavior_nudge::observe(
        name,
        args,
        crate::core::tokens::count_tokens(&result_text),
        !machine_readable && !firewalled,
    ) {
        result_text.push_str("\n\n");
        result_text.push_str(&hint);
    }

    // Resolve the active profile once per dispatch: it is stable for the
    // lifetime of a single tool call, and `active_profile()` is an expensive
    // resolve (config load + disk reads + inheritance merge). Reused below
    // for the verify footer and the auto-checkpoint marker.
    let active_profile = crate::core::profiles::active_profile();
    let profile_hints = active_profile.output_hints.clone();

    if !is_raw_shell && !firewalled && profile_hints.verify_footer() {
        let verify_cfg = active_profile.verification;
        let vr = crate::core::output_verification::verify_output(
            &pre_compression,
            &result_text,
            &verify_cfg,
        );
        if !vr.warnings.is_empty() {
            let msg = format!("[VERIFY] {}", vr.format_compact());
            result_text = format!("{result_text}\n\n{msg}");
        }
    }

    // Raw output stays byte-pure: the body is archived (ctx_expand works),
    // but the hint decoration is only appended to non-raw deliveries.
    // #1660: remembered, not just appended. Truncation below keeps a prefix,
    // so a hint pinned to the end of the body was the first thing cut — the
    // response then said it was truncated while the line naming the archived
    // copy had already been removed.
    let mut recovery_line: Option<String> = None;
    if !is_raw_shell
        && !firewalled
        && profile_hints.archive_hint()
        && let Some(hint) = archive_hint
    {
        result_text = format!("{result_text}\n{hint}");
        recovery_line = Some(hint);
    }

    // These legacy decorations have no original-source authority. Output-only
    // redaction cannot recover classification labels absent from a graph hint.
    let legacy_context_allowed = !crate::core::policy::runtime::is_active();
    let auto_context = auto_context.filter(|_| legacy_context_allowed);
    let had_auto_context = auto_context.is_some();
    let mut had_legacy_context = had_auto_context;
    let had_throttle_warning = throttle_warning.is_some();

    // ═══════════════════════════════════════════════════════════════════════
    // Provider-Cache Zones (#E26)
    //
    // Tool output = STABLE ZONE + DYNAMIC ZONE
    //
    // STABLE ZONE: The core tool result from dispatch (result_text at this
    //   point). Deterministic for same input — verified by #498 tests.
    //
    // DYNAMIC ZONE: Session-dependent decorations appended/prepended below.
    //
    // auto_context (the wake-up briefing) and the shared-context hint are
    // session-specific; they decorate only `ctx_session` load/status/show
    // (`delivers_session_briefing`), never a read, search or compose result.
    // ═══════════════════════════════════════════════════════════════════════
    if !is_raw_shell && let Some(ctx) = auto_context {
        let ctx_tokens = crate::core::tokens::count_tokens(&ctx);
        if ctx_tokens <= crate::tools::autonomy::AUTO_CONTEXT_MAX_TOKENS {
            result_text = format!("{ctx}\n\n{result_text}");
        }
    }

    // Shared memory is partitioned by project: only the request project's own
    // facts may decorate this result.
    let hint_root = if !is_raw_shell
        && legacy_context_allowed
        && crate::tools::autonomy::delivers_session_briefing(name, args)
    {
        match crate::core::policy::diagnostics::request_project() {
            Some(root) => Some(root.to_string_lossy().into_owned()),
            None => server.session.read().await.project_root.clone(),
        }
    } else {
        None
    };
    if let Some(root) = hint_root
        && let Some(hint) = crate::core::shared_context::session_start_hint(&root)
    {
        had_legacy_context = true;
        result_text = format!("{hint}\n\n{result_text}");
    }
    if let Some(warning) = throttle_warning {
        result_text = format!("{result_text}\n\n{warning}");
    }

    // Additive, best-effort reference advice. Resolver failures become empty
    // advice, so normal Context Gate output is never affected.
    if legacy_context_allowed && matches!(name, "ctx_read" | "ctx_search" | "ctx_compose") {
        let query = helpers::get_str(args, "query")
            .or_else(|| helpers::get_str(args, "task"))
            .unwrap_or_default();
        if !query.is_empty() {
            let advice = context_gate::knowledge_advice(&query);
            if let Some(hint) = advice.additional_context_hint {
                had_legacy_context = true;
                result_text = format!("{result_text}\n\n{hint}");
            }
        }
    }

    // Gated on `!machine_readable` (short-circuits before the swap) so a
    // json-first call does not consume this once-per-session slot for a tip
    // we would immediately discard; it then surfaces on the next call.
    if !machine_readable
        && !server
            .rules_stale_checked
            .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        let client = server.client_name.read().await.clone();
        if !client.is_empty() && crate::rules_inject::check_rules_freshness(&client).is_some() {
            // Self-heal: auto-refresh the rules on disk instead of asking
            // the user to run setup manually (#2365). The rewrite is
            // idempotent and cheap; run it off the async runtime.
            let heal_result = tokio::task::spawn_blocking(|| {
                dirs::home_dir().map(|home| crate::rules_inject::inject_all_rules(&home))
            })
            .await
            .ok()
            .flatten();

            result_text = append_rules_self_heal_status(result_text, heal_result.as_ref());
        } else if !server
            .rules_tip_shown
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            let cfg = crate::core::config::Config::load();
            // #1754: this tip is lean-ctx steering delivered inside a tool
            // result, so the settings that decline steering must silence it.
            // The tip is for hosts whose guidance is really missing: Claude
            // Code keeps it in CLAUDE.md, which no rules target covers, so
            // `should_inject_rules()` alone told set-up Claude users to run
            // setup in every session.
            if !cfg.declines_rule_steering()
                && !cfg.setup.should_inject_rules()
                && !dirs::home_dir()
                    .is_some_and(|home| crate::rules_inject::host_guidance_present(&home))
            {
                result_text = format!(
                    "{result_text}\n\n\
                         --- tip: run 'lean-ctx setup' to configure agent rules for optimal AI integration ---"
                );
            }
        }
    }

    let mut ledger_intent = None;
    if name == "ctx_read" {
        if let Some(read_path) = args
            .as_ref()
            .and_then(|args| args.get("path"))
            .and_then(serde_json::Value::as_str)
        {
            let task_class = task_profile
                .as_ref()
                .map_or("unknown", |profile| profile.task_class.as_str());
            let task_class = task_class.to_owned();
            let read_path = read_path.to_owned();
            let project_root = {
                let session = server.session.read().await;
                session.project_root.clone()
            };
            tokio::spawn(async move {
                let predictions = crate::server::predictive_preload::record_read_and_predict(
                    &task_class,
                    &read_path,
                );
                crate::server::predictive_preload::warm_paths(
                    project_root.as_deref().map(std::path::Path::new),
                    &predictions,
                );
            });
        }
        let read_path = server
            .resolve_path_or_passthrough(&helpers::get_str(args, "path").unwrap_or_default())
            .await;
        let project_root = server.session.read().await.project_root.clone();
        // Continuity is independent of minimal output.
        // Directory overviews are not file reads (GL #512).
        if std::path::Path::new(&read_path).is_file() {
            ledger_intent = Some(McpLedgerIntent {
                read_path,
                mode_used: helpers::get_str(args, "mode").unwrap_or_else(|| "auto".to_string()),
                output_tokens: output_tokens as usize,
                sent_tokens: crate::core::tokens::count_tokens(&result_text),
                project_root,
                wants_elicitation: profile_hints.elicitation_hint(),
            });
        }
    }

    if !minimal && !is_raw_shell && name == "ctx_shell" {
        let cmd = helpers::get_str(args, "command").unwrap_or_default();

        if profile_hints.efficiency_hint() {
            let calls = server.tool_calls.read().await;
            let last_original = calls.last().map_or(0, |c| c.original_tokens);
            drop(calls);
            let pre_hint_tokens = crate::core::tokens::count_tokens(&result_text);
            if let Some(hint) = crate::tools::autonomy::shell_efficiency_hint(
                &server.autonomy,
                &cmd,
                last_original,
                pre_hint_tokens,
            ) {
                result_text = format!("{result_text}\n{hint}");
            }
        }
    }

    // Bypass hints are decoupled from minimal_overhead: they ride MCP
    // tool responses (which vary anyway) and don't break provider prompt
    // caching (#498). The `bypass_hints` config key gates them independently.
    if !is_raw_shell && bypass_hint::is_enabled() {
        if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
            let session = server.session.read().await;
            bypass_hint::set_session_id(&session.id);
            drop(session);
            if let Some(hint) = bypass_hint::check(&data_dir) {
                result_text = format!("{result_text}\n{hint}");
            }
        }
        bypass_hint::record_lctx_call();
    }

    let finding_path_hint = helpers::get_str(args, "path");
    if let Some(finding) =
        crate::core::auto_findings::extract(name, &findings_source, finding_path_hint.as_deref())
    {
        let mut session = server.session.write().await;
        let current = crate::core::policy::runtime::active();
        let safe = crate::core::policy::diagnostics::inspect(
            &serde_json::json!({"file":finding.file,"summary":finding.summary}),
            current.as_deref(),
        );
        if let Some(safe) = safe
            && current.as_ref().is_none_or(|policy| {
                policy.tool_allowed(name)
                    && diagnostic_fields
                        .as_ref()
                        .and_then(|record| record["result"].as_str())
                        .is_some_and(|original| {
                            !crate::core::policy::content::evaluate_text(original, policy).blocked
                        })
            })
        {
            // Bound to the call's sources only when the tool declares them all.
            let declared = tool_declaration(server, name, args).publishes_every_source;
            let (file, summary) = (safe["file"].as_str(), safe["summary"].as_str());
            let sources = archive_authority.as_deref().filter(|_| declared);
            session.add_finding_from_call(file, summary.unwrap_or_default(), sources);
        }
        let project_root = crate::core::policy::diagnostics::request_project()
            .map(|root| root.to_string_lossy().into_owned())
            .or_else(|| session.project_root.clone());
        drop(session);
        if let Some(ref root) = project_root {
            let f = finding.clone();
            let r = root.clone();
            let source_tool = name.to_owned();
            let source_record = diagnostic_fields.clone();
            crate::core::policy::runtime::spawn_for_project(r.clone(), move || {
                let Some(record) = source_record else { return };
                let Some(original) = record["result"].as_str() else {
                    return;
                };
                crate::core::auto_capture::capture_finding_from_tool(
                    &r,
                    &f,
                    &source_tool,
                    original,
                );
            });
        }
    }
    if let Some(extra) = crate::core::auto_capture::extract_extra(name, &findings_source) {
        let session = server.session.read().await;
        let project_root = crate::core::policy::diagnostics::request_project()
            .map(|root| root.to_string_lossy().into_owned())
            .or_else(|| session.project_root.clone());
        drop(session);
        if let Some(ref root) = project_root {
            let e = extra.clone();
            let r = root.clone();
            let source_tool = name.to_owned();
            let source_record = diagnostic_fields.clone();
            crate::core::policy::runtime::spawn_for_project(r.clone(), move || {
                let Some(record) = source_record else { return };
                let Some(original) = record["result"].as_str() else {
                    return;
                };
                crate::core::auto_capture::capture_finding_from_tool(
                    &r,
                    &e,
                    &source_tool,
                    original,
                );
            });
        }
    }

    {
        let tool_name = name.to_string();
        // Retain complete text until the writer checks current policy. A block
        // marker on a later line must not disappear in a preview first.
        let diagnostic_root = crate::core::policy::diagnostics::request_project();
        let diagnostic_root = match diagnostic_root {
            Some(root) => Some(root),
            None => server
                .session
                .read()
                .await
                .project_root
                .as_ref()
                .map(std::path::PathBuf::from),
        };
        // #520 opt-in debug log: a full per-call record (tool, args, result
        // preview, savings, wall time). Captured here and written off the hot
        // path in the existing journal thread; no-op unless `debug_log` is on.
        let dbg_bytes = result_text.len();
        let dbg_saved = tool_saved_tokens;
        let dbg_elapsed = tool_start.elapsed();
        std::thread::spawn(move || {
            let Some(root) = diagnostic_root else { return };
            let Some(record) = diagnostic_fields else {
                return;
            };
            let Some(diagnostic_text) = record["result"].as_str() else {
                return;
            };
            let dbg_args = record.get("args").and_then(serde_json::Value::as_object);
            crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
                std::cell::RefCell::new(Some(root)),
                || {
                    crate::core::journal::maybe_day_separator();
                    crate::core::journal::log_tool_call(&tool_name, diagnostic_text);
                    crate::core::debug_log::log_mcp_call(
                        &tool_name,
                        dbg_args,
                        diagnostic_text,
                        dbg_bytes,
                        dbg_saved,
                        dbg_elapsed,
                    );
                },
            );
        });
    }

    // OPT-4: dispatch/mod.rs records savings before terse/hints run; this
    // finalizes the real sent-token count and corrects persistent stats.
    let output_token_count = post_process::finalize_token_count_and_adjust(
        name,
        &result_text,
        pre_terse_len,
        output_tokens,
        tool_saved_tokens,
    );
    record_compression_savings(name, tool_saved_tokens, output_token_count);

    let action = helpers::get_str(args, "action");

    // K-bounded staleness guard: warn if shared context has diverged.
    const K_STALENESS_BOUND: i64 = 10;
    if server.session_mode == crate::tools::SessionMode::Shared
        && let Some(ref rt) = server.context_os
    {
        let latest = rt.bus.latest_id(&server.workspace_id, &server.channel_id);
        let cursor = server
            .last_seen_event_id
            .load(std::sync::atomic::Ordering::Relaxed);
        if cursor > 0 && latest - cursor > K_STALENESS_BOUND {
            let gap = latest - cursor;
            result_text = format!(
                "[CONTEXT STALE] {gap} events happened since your last read. \
                         Use ctx_session(action=\"status\") to sync.\n\n{result_text}"
            );
        }
        server
            .last_seen_event_id
            .store(latest, std::sync::atomic::Ordering::Relaxed);
    }

    let receipt_intent = Some(McpReceiptIntent {
        name: name.to_owned(),
        args: args.cloned(),
        action: action.clone(),
        result_text: result_text.clone(),
        output_token_count,
    });

    // Context Bus: conflict detection for knowledge writes in shared mode.
    if server.session_mode == crate::tools::SessionMode::Shared
        && name == "ctx_knowledge"
        && action.as_deref() == Some("remember")
        && let Some(ref rt) = server.context_os
    {
        let my_agent = server.agent_id.read().await.clone();
        let category = helpers::get_str(args, "category");
        let key = helpers::get_str(args, "key");
        if let (Some(cat), Some(k)) = (&category, &key) {
            let recent = rt.bus.recent_by_kind(
                &server.workspace_id,
                &server.channel_id,
                "knowledge_remembered",
                20,
            );
            for ev in &recent {
                let p = &ev.payload;
                let ev_cat = p.get("category").and_then(|v| v.as_str());
                let ev_key = p.get("key").and_then(|v| v.as_str());
                let ev_actor = ev.actor.as_deref();
                if ev_cat == Some(cat.as_str())
                    && ev_key == Some(k.as_str())
                    && ev_actor != my_agent.as_deref()
                {
                    let other = ev_actor.unwrap_or("unknown");
                    result_text = format!(
                        "[CONFLICT] Agent '{other}' recently wrote to the same knowledge key \
                                 '{cat}/{k}'. Review before proceeding.\n\n{result_text}"
                    );
                    break;
                }
            }
        }
    }

    server
        .persist_shared_context_os(name, action.as_deref(), args)
        .await;

    if let Some(crate::server::tool_trait::ShellOutcome::Background(outcome)) =
        shell_outcome.as_ref()
        && let Some(display) = &outcome.display
    {
        let mut rendered = display.header.clone();
        if !result_text.trim().is_empty() {
            rendered.push('\n');
            rendered.push_str(&result_text);
        }
        if let Some(footer) = &display.footer {
            rendered.push('\n');
            rendered.push_str(footer);
        }
        result_text = rendered;
    }

    // Machine-readable bodies are restored byte-exact further down, but the
    // auto-checkpoint returns before that restore — so it (and the once-per-
    // session nudges, which would be thrown away) must not run for them. The
    // flag is computed from the inner tool, so this also covers `ctx_call`.
    let skip_checkpoint = minimal
        || machine_readable
        || matches!(
            name,
            "ctx_compress"
                | "ctx_metrics"
                | "ctx_discover"
                | "ctx_dedup"
                | "ctx_session"
                | "ctx_knowledge"
                | "ctx_agent"
                | "ctx_share"
                | "ctx_gain"
                | "ctx_preload"
                | "ctx_cost"
                | "ctx_heatmap"
        );

    // Output-echo nudge (#501): when the agent keeps re-quoting delivered
    // content, tell it once (cooldown-limited, stable text per #498).
    if !skip_checkpoint
        && crate::core::protocol::meta_visible()
        && let Some(nudge) = crate::core::output_echo::take_pending_nudge()
    {
        result_text.push_str(&nudge);
    }

    // Proactive update nudge: when the running MCP binary is behind the
    // latest release, surface it to the agent once per session (stable text
    // per #498, read from the local cache the background check fills at
    // server start). Notify-only — it never auto-installs and honors
    // `update_check_disabled` / `LEAN_CTX_NO_UPDATE_CHECK`.
    if !skip_checkpoint
        && crate::core::protocol::meta_visible()
        && let Some(hint) = crate::core::version_check::session_update_hint()
    {
        result_text.push_str("\n\n");
        result_text.push_str(&hint);
    }

    // #1020: tool-calls.log is now written on the dispatch path
    // (record_call_with_path / record_call_with_timing) with the real
    // original/saved/mode and the measured handler duration. The previous
    // zero-filled append here overwrote every row with `orig=0 saved=0 mode=-`.

    // The cloud/telemetry flush is scheduled at the top of `call_tool_guarded`
    // (see `background_tick`): this function is skipped for guard-denied and
    // response-cached calls, so counting here would miss them. Archive cleanup
    // stays on the dispatch path, where it only has work when a call actually
    // produced output.
    let current_count = server
        .background_tick
        .load(std::sync::atomic::Ordering::Relaxed);
    if current_count > 0 && current_count.is_multiple_of(100) {
        // Bound the on-disk archive between restarts: prune TTL-expired and
        // over-budget entries off the hot path so it can't grow unbounded and
        // starve the host of RAM via the page cache (#417).
        std::thread::spawn(|| {
            let _ = crate::core::archive::cleanup();
        });
    }

    // #990: a machine-readable invocation (e.g. ctx_outline format=json) must
    // return a byte-exact, parseable payload. The state-consuming briefings
    // are already skipped above (so their once-per-session flags survive),
    // but other steps still append recomputed prose (verify footer, throttle
    // / budget warning, deprecation notice) or compress the body — all of
    // which break a JSON contract. This guard is the robust catch-all:
    // restore the pure body captured *before* compression and decoration.
    // Redaction + sensitivity were applied earlier so the security envelope
    // is preserved. `ctx_outline` is not an archivable/firewallable tool, so
    // `pre_compression` is the unmodified body here; no-op otherwise.
    if machine_readable {
        result_text = pre_compression;
    }

    // Turn-level budget enforcement (#1306), including the limit choice and the
    // recovery line that has to survive the cut. It lives in `core::budget`
    // because it is budget policy, not pipeline sequencing.
    result_text = crate::core::budget::enforce_turn_budget(
        &result_text,
        verbatim_requested(delivery_name, delivery_args),
        recovery_line.as_deref(),
        name,
    );

    // Decorations can contain recovered session/source text. Inspect the final
    // body again rather than treating trusted formatting as trusted content.
    if had_legacy_context && crate::core::policy::runtime::is_active() {
        return Ok(policy_blocked_output(
            "[POLICY BLOCKED] Source policy changed while legacy context was being assembled.",
        ));
    }
    result_text = match policy_guard::protect_result(name, &result_text) {
        Ok(text) => text,
        Err(reason) => return Ok(policy_blocked_output(reason)),
    };
    let compressed_input_tokens = crate::core::tokens::count_tokens(&result_text) as u64;
    crate::tools::ctx_provider::ensure_snapshot_output(name, args, &result_text)?;
    record_shell_error_category(shell_outcome.as_ref(), &result_text);
    let mut result = finalize_call_result(&result_text, shell_outcome);
    if let Some(category) = tool_error_category {
        result.is_error = Some(true);
        record_error_category(category);
    }
    // Delivered-content lineage (G03/G04): this is the release point — the
    // text blocks of `result` are the bytes the client receives (a later
    // checkpoint is a block of its own). Each is registered under its digest
    // with the call's source set, so the proxy can re-authorize the same
    // bytes when they come back. Only a COMPLETE set of a declared tool is
    // recorded, and nothing when session-derived context without source
    // authority was mixed into the text: unknown stays unknown. The
    // conditions live in `record_tool_delivery`, shared with the CLI path;
    // this site only states what the pipeline itself added to the text.
    let declares_every_source = !had_legacy_context
        && ((name == "ctx_execute" && helpers::get_str(args, "action").as_deref() == Some("file"))
            || server.registry.as_deref().is_some_and(|registry| {
                crate::core::archive::delivered::declares_every_source(registry, name, args)
            }));
    let delivered_blocks: Vec<&str> = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect();
    crate::server::call_identity::record_delivery(
        declares_every_source,
        tool_error || result.is_error == Some(true),
        archive_authority.as_deref().cloned(),
        &delivered_blocks,
    );
    let has_dynamic = had_auto_context || had_throttle_warning;
    let mut meta = rmcp::model::Meta::new();
    meta.0.insert(
        "cache_hint".to_owned(),
        serde_json::Value::String(if has_dynamic { "ephemeral" } else { "stable" }.to_owned()),
    );
    result.meta = Some(meta);

    // Account only for the final emitted body so admission checks use the
    // same token count that was delivered to the agent.
    let agent_id = if let Some(agent_id) = server.agent_id.read().await.clone() {
        agent_id
    } else {
        server
            .presence_agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "mcp-agent".to_owned())
    };
    crate::core::agent_budget::record_consumption(
        &agent_id,
        usize::try_from(compressed_input_tokens).unwrap_or(usize::MAX),
    );
    crate::core::agent_budget::record_turn_delivery(&agent_id, compressed_input_tokens);
    Ok(McpProcessed {
        result,
        ir: ir_intent,
        ledger: ledger_intent,
        receipt: receipt_intent,
        checkpoint: Some(McpCheckpointIntent {
            enabled: !skip_checkpoint,
            output_visible: profile_hints.checkpoint_in_output(),
        }),
    })
}

#[path = "pipeline_records.rs"]
mod records;
pub(in crate::server) use records::{record_checkpoint, record_context_ir, record_ledger};

#[path = "pipeline_telemetry.rs"]
mod telemetry;

#[cfg(test)]
use telemetry::shell_error_category;
pub(in crate::server) use telemetry::{error_failure, result_failure};
use telemetry::{
    mcp_error_category, record_error_category, record_shell_error_category,
    tool_execution_error_category,
};

#[path = "pipeline_shell_effects.rs"]
mod shell_effects;
use shell_effects::{record_shell_file_effects, shell_command_base_dir};

#[path = "pipeline_triage.rs"]
mod triage;
use triage::apply_task_triage_filter;
pub(super) use triage::{triage_bypass_requested, verbatim_requested};

#[path = "pipeline_terminal.rs"]
mod terminal;
use terminal::{policy_blocked_output, protect_terminal_result};

#[path = "pipeline_savings.rs"]
mod savings;
#[cfg(test)]
use savings::compression_tracker_tokens;
use savings::record_compression_savings;

#[cfg(test)]
#[cfg_attr(windows, allow(unused_imports))] // its only consumer is a cfg(not(windows)) test
pub(in crate::server) use test_driver::dispatch_and_post_process;

#[cfg(test)]
#[path = "pipeline_savings_tests.rs"]
mod savings_tests;

#[cfg(test)]
#[path = "pipeline_policy_tests.rs"]
mod policy_tests;

#[cfg(test)]
#[path = "pipeline_test_driver.rs"]
mod test_driver;
