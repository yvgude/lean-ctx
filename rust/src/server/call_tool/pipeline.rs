// SPDX-License-Identifier: Apache-2.0

#[allow(clippy::wildcard_imports)]
use super::super::*;
use super::{append_rules_self_heal_status, finalize_call_result};

pub(in crate::server) enum McpPrimitive {
    Raw(McpRawOutput),
    Terminal(CallToolResult, &'static str),
}

impl McpPrimitive {
    /// Whether the tool itself succeeded — what usage telemetry counts.
    pub(in crate::server) fn succeeded(&self) -> bool {
        match self {
            Self::Raw(raw) => !raw.tool_error,
            Self::Terminal(result, _) => result.is_error != Some(true),
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

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
#[cfg_attr(windows, allow(dead_code))] // its only consumer is a cfg(not(windows)) test
pub(in crate::server) async fn dispatch_and_post_process(
    server: &LeanCtxServer,
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
    minimal: bool,
    config: std::sync::Arc<crate::core::config::Config>,
    machine_readable: bool,
    auto_context: Option<String>,
    throttle_warning: Option<String>,
    args_fp: String,
    decision_context: Option<crate::core::decision_loop_runtime::TaskContext>,
) -> Result<CallToolResult, ErrorData> {
    let raw = dispatch_primitive(server, name, args, minimal, &args_fp).await?;
    let mut processed = reversible_post_process(
        server,
        name,
        args,
        minimal,
        config,
        machine_readable,
        auto_context,
        throttle_warning,
        decision_context,
        raw,
    )
    .await?;
    let _ = record_context_ir(server, &processed).await;
    record_ledger(server, &processed).await?;
    let checkpoint = processed.checkpoint.take();
    let _ = record_checkpoint(server, checkpoint, &mut processed.result).await;
    Ok(processed.result)
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
    let (dispatched, archive_authority) =
        crate::core::archive::authority::capture(server.dispatch_tool(name, args, minimal)).await;
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
    let output_tokens = {
        let tokens = crate::core::tokens::count_tokens(&result_text) as u64;
        crate::core::budget_tracker::BudgetTracker::global().record_tokens(tokens);
        tokens
    };

    crate::core::anomaly::record_metric("tokens_per_call", output_tokens as f64);

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
        } else if name == "ctx_expand" || name == "ctx_retrieve" {
            // CCR-learning (#941): a verbatim/original re-fetch means the inline
            // compressed form was too lossy for this session.
            detector.record_retrieve();
        }
        let correction_count = detector.correction_count();
        let retrieve_count = detector.retrieve_count();
        if correction_count > 0 {
            crate::core::anomaly::record_metric(
                "correction_loop_rate",
                f64::from(correction_count),
            );
        }
        if retrieve_count > 0 {
            crate::core::anomaly::record_metric("ccr_retrieve_rate", f64::from(retrieve_count));
        }
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

    // Persist anomaly detector — debounced to reduce I/O in burst sequences.
    crate::core::anomaly::save_debounced();

    let budget_warning = post_process::budget_warning_message();

    // Out-of-band archive + optional context firewall for large tool outputs.
    // For firewallable tools (ctx_shell/ctx_execute/ctx_search/ctx_tree) whose output
    // exceeds the ephemeral threshold, the full (redacted) body is stored out-of-band
    // and the inline result is replaced by a compact digest + ctx_expand drilldown.
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
                "ctx_shell"
                    | "ctx_read"
                    | "ctx_multi_read"
                    | "ctx_smart_read"
                    | "ctx_execute"
                    | "ctx_search"
                    | "ctx_tree"
            );
            if archivable && archive::should_archive(&result_text) {
                let cmd = helpers::get_str(args, "command")
                    .or_else(|| helpers::get_str(args, "path"))
                    .unwrap_or_default();
                let session_id = server.session.read().await.id.clone();
                let to_store = crate::core::redaction::redact_text_if_enabled(&result_text);
                let tokens = crate::core::tokens::count_tokens(&to_store);
                // Only the admitted file handler currently supplies complete archive
                // source provenance. Other output classes must not borrow its binding.
                let authority = (name == "ctx_execute"
                    && helpers::get_str(args, "action").as_deref() == Some("file"))
                .then_some(archive_authority.as_deref())
                .flatten();
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

    // Echo-ratio nudge (#science): when output largely repeats the task
    // description, surface a deterministic hint before footer markers (#498).
    if !machine_readable
        && !is_raw_shell
        && !firewalled
        && crate::core::cognitive_gate::full_science_enabled()
        && !result_text.is_empty()
    {
        let task_input = {
            let session = server.session.read().await;
            session.task.as_ref().map(|t| t.description.clone())
        };
        if let Some(task) = task_input.filter(|t| !t.is_empty()) {
            let report = crate::core::echo_ratio::compute_echo_ratio(&task, &result_text);
            if report.ratio > 0.7 {
                result_text.push_str(&format!(
                    "\n[cognitive: high echo ratio ({:.0}%) — consider generating novel content]",
                    report.ratio * 100.0
                ));
            }
        }
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
    let had_budget_warning = budget_warning.is_some();
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
    // KNOWN ISSUE: auto_context is PREPENDED (line below), which places
    // session-specific content at the START of the tool result. This breaks
    // provider prefix caching for the first tool call of each session. Since
    // auto_context fires only once per session (gated), this is acceptable:
    // the first call is always a cache miss anyway. All subsequent calls have
    // a stable prefix.
    //
    // Future optimization: move auto_context to MCP _meta or a separate
    // notification channel to preserve the stable prefix even on first call.
    // ═══════════════════════════════════════════════════════════════════════
    if !is_raw_shell && let Some(ctx) = auto_context {
        let ctx_tokens = crate::core::tokens::count_tokens(&ctx);
        if ctx_tokens <= 400 {
            result_text = format!("{ctx}\n\n{result_text}");
        }
    }

    if !is_raw_shell
        && legacy_context_allowed
        && name != "ctx_memory"
        && let Some(hint) = crate::core::shared_context::session_start_hint()
    {
        had_legacy_context = true;
        result_text = format!("{hint}\n\n{result_text}");
    }
    if let Some(warning) = throttle_warning {
        result_text = format!("{result_text}\n\n{warning}");
    }

    if let Some(bw) = budget_warning {
        result_text = format!("{result_text}\n\n{bw}");
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
            if !cfg.declines_rule_steering() && !cfg.setup.should_inject_rules() {
                result_text = format!(
                    "{result_text}\n\n\
                         --- tip: run 'lean-ctx setup' to configure agent rules for optimal AI integration ---"
                );
            }
        }
    }

    {
        // Evaluate SLOs for observability (watch/dashboard), but keep tool outputs clean.
        let _ = crate::core::slo::evaluate();
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
        if minimal {
            let cache_clone = server.cache.clone();
            let autonomy_clone = server.autonomy.clone();
            let name_owned = name.to_string();
            tokio::spawn(async move {
                let result = std::panic::AssertUnwindSafe(async {
                    // #807: bounded lock — the old unbounded `.write().await`
                    // could queue behind a long computation and then hold the
                    // lock during dedup, cascading the stall.
                    let cache_timeout = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        cache_clone.write(),
                    )
                    .await;
                    if let Ok(mut cache) = cache_timeout {
                        crate::tools::autonomy::maybe_auto_dedup(
                            &autonomy_clone,
                            &mut cache,
                            &name_owned,
                        );
                    } else {
                        tracing::debug!("background auto_dedup: cache lock timeout (5s), skipping");
                    }
                })
                .catch_unwind()
                .await;
                if let Err(e) = result {
                    let msg = e
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| e.downcast_ref::<&str>().copied())
                        .unwrap_or("unknown");
                    tracing::error!("background auto_dedup panicked: {msg}");
                }
            });
        } else {
            // Bounded cache lock for enrichment — degrade gracefully under contention
            let enrich_timeout =
                tokio::time::timeout(std::time::Duration::from_secs(3), server.cache.write()).await;
            if let Ok(mut cache) = enrich_timeout {
                let enrich = crate::tools::autonomy::enrich_after_read(
                    &server.autonomy,
                    &mut cache,
                    &read_path,
                    project_root.as_deref(),
                    None,
                    crate::tools::CrpMode::effective(),
                    false,
                );
                if profile_hints.related_hint()
                    && let Some(hint) = enrich.related_hint
                {
                    result_text = format!("{result_text}\n{hint}");
                }
                crate::tools::autonomy::maybe_auto_dedup(&server.autonomy, &mut cache, name);
            } else {
                tracing::warn!(
                    "post-dispatch cache lock timeout (3s) for {read_path}, skipping enrichment"
                );
            }
        }
        // Continuity is independent of optional enrichment/minimal output.
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

        if let Some(file_path) = extract_file_read_from_shell(&cmd)
            && let Ok(mut bt) = crate::core::bounce_tracker::global().lock()
        {
            bt.next_seq();
            bt.record_shell_file_access(&file_path);
        }

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
            session.add_finding(
                safe["file"].as_str(),
                None,
                safe["summary"].as_str().unwrap_or_default(),
            );
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
            std::thread::spawn(move || {
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
            std::thread::spawn(move || {
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
                | "ctx_benchmark"
                | "ctx_analyze"
                | "ctx_cache"
                | "ctx_discover"
                | "ctx_dedup"
                | "ctx_session"
                | "ctx_knowledge"
                | "ctx_agent"
                | "ctx_share"
                | "ctx_gain"
                | "ctx_overview"
                | "ctx_preload"
                | "ctx_cost"
                | "ctx_heatmap"
                | "ctx_task"
                | "ctx_impact"
                | "ctx_architecture"
                | "ctx_smells"
                | "ctx_quality"
                | "ctx_workflow"
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
    // and knowledge consolidation stay on the dispatch path, where they only
    // have work when a call actually produced output.
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
        // Self-managing memory: opportunistically consolidate knowledge in the
        // background (time-gated + single-flight inside `maybe_run`).
        if let Some(root) = server.session.read().await.project_root.clone() {
            crate::core::cognition_scheduler::maybe_run(&root);
        }
    }

    // #509: a folded read-cluster alias (ctx_smart_read / ctx_multi_read) stays
    // callable but warns — prepend a one-line notice steering to the primary.
    if let Some(notice) = crate::server::dynamic_tools::deprecation_notice(name) {
        result_text = format!("{notice}\n{result_text}");
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
    if tool_error {
        result.is_error = Some(true);
        record_error_category(crate::core::telemetry_v2::ErrorCategory::Internal);
    }
    let has_dynamic = had_auto_context || had_budget_warning || had_throttle_warning;
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

#[path = "pipeline_terminal.rs"]
mod terminal;
use terminal::{policy_blocked_output, protect_terminal_result};

#[cfg(test)]
use telemetry::shell_error_category;
use telemetry::{mcp_error_category, record_error_category, record_shell_error_category};

#[path = "pipeline_triage.rs"]
mod triage;
use triage::apply_task_triage_filter;
pub(super) use triage::{triage_bypass_requested, verbatim_requested};

/// Record real compression savings after every response rewrite is complete.
///
/// Registered tools report the raw-to-tool-output delta in `tool_saved_tokens`.
/// The final output count includes dispatcher-level terse compression and
/// decorations. This is deliberately best-effort: tracker failures cannot
/// affect a completed tool response.
fn record_compression_savings(name: &str, tool_saved_tokens: usize, output_tokens: usize) {
    if let Some((raw_tokens, compressed_tokens)) =
        compression_tracker_tokens(name, tool_saved_tokens, output_tokens)
    {
        crate::core::savings_tracker::record_compression(raw_tokens, compressed_tokens, name);
    }
}

fn compression_tracker_tokens(
    name: &str,
    tool_saved_tokens: usize,
    output_tokens: usize,
) -> Option<(u64, u64)> {
    matches!(
        name,
        "ctx_read" | "ctx_shell" | "ctx_search" | "ctx_compose"
    )
    .then(|| {
        let raw_tokens = tool_saved_tokens.saturating_add(output_tokens) as u64;
        (raw_tokens, output_tokens as u64)
    })
}

#[cfg(test)]
#[path = "pipeline_savings_tests.rs"]
mod savings_tests;

#[cfg(test)]
#[path = "pipeline_policy_tests.rs"]
mod policy_tests;
