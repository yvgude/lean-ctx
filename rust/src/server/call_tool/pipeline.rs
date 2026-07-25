#[allow(clippy::wildcard_imports)]
use super::super::*;
use super::finalize_call_result;

#[allow(clippy::too_many_arguments)]
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
) -> Result<CallToolResult, ErrorData> {
    let tool_start = std::time::Instant::now();
    let (mut result_text, tool_saved_tokens, shell_outcome, content_blocks) =
        match server.dispatch_tool(name, args, minimal).await {
            Ok(tuple) => tuple,
            Err(e) => {
                if let Ok(mut detector) = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    server.loop_detector.write(),
                )
                .await
                {
                    detector.record_error_outcome(name, &args_fp);
                }
                crate::core::debug_log::log_mcp_error(name, args, &format!("{e:?}"));
                return Err(e);
            }
        };

    // Image/binary content blocks: skip all post-processing, return directly.
    if let Some(blocks) = content_blocks {
        let mut result = CallToolResult::success(blocks);
        if let Some(outcome) = shell_outcome
            && outcome.is_error()
        {
            result.is_error = Some(true);
        }
        return Ok(result);
    }

    let inline_shell = name == "ctx_shell"
        && crate::core::firewall::should_inline_shell(
            helpers::get_bool(args, "inline").unwrap_or(false),
            result_text.len(),
            &config,
        );
    let is_raw_shell = name == "ctx_shell" && {
        let arg_raw = helpers::get_bool(args, "raw").unwrap_or(false);
        let arg_bypass = helpers::get_bool(args, "bypass").unwrap_or(false);
        arg_raw
            || arg_bypass
            || std::env::var("LEAN_CTX_DISABLED").is_ok()
            || crate::core::runtime_flags::raw_enabled()
            || inline_shell
            // #1260: dataset output (sqlite3/psql/jq/`gh --json`) is destroyed,
            // not compressed, by middle elision — pass it through at any size.
            || helpers::get_str(args, "command")
                .is_some_and(|c| crate::core::firewall::is_raw_command(&c, &config))
    };

    let pre_terse_len = result_text.len();
    let output_tokens = {
        let tokens = crate::core::tokens::count_tokens(&result_text) as u64;
        crate::core::budget_tracker::BudgetTracker::global().record_tokens(tokens);
        tokens
    };

    crate::core::anomaly::record_metric("tokens_per_call", output_tokens as f64);

    // Context IR: record lineage for every tool call.
    if let Some(ref ir) = server.context_ir {
        let tool_duration = tool_start.elapsed();
        let source_kind = post_process::context_ir_source_kind(name);
        let ir_path = helpers::get_str(args, "path");
        let ir_command = helpers::get_str(args, "command");
        let ir_mode = helpers::get_str(args, "mode");
        let excerpt = if result_text.len() > 200 {
            let mut end = 200;
            while !result_text.is_char_boundary(end) && end > 0 {
                end -= 1;
            }
            &result_text[..end]
        } else {
            &result_text
        };
        let input = crate::core::context_ir::RecordIrInput {
            kind: source_kind,
            tool: name,
            client_name: None,
            agent_id: None,
            path: ir_path.as_deref(),
            command: ir_command.as_deref(),
            pattern: ir_mode.as_deref(),
            input_tokens: pre_terse_len / 4,
            output_tokens: output_tokens as usize,
            duration: tool_duration,
            content_excerpt: excerpt,
        };
        ir.write().await.record(input);
    }

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

    // #212 — per-item sensitivity floor. Enforced uniformly here (before
    // archiving + compression) so it covers both the inline result and the
    // out-of-band copy. No-op unless `sensitivity.enabled` (default off)
    // or the active persona declares a floor above `public`
    // (persona-spec-v1: e.g. `lead-gen` enforces `confidential`).
    {
        let path_hint = helpers::get_str(args, "path");
        let enforced = crate::core::sensitivity::enforce_text(
            std::mem::take(&mut result_text),
            path_hint.as_deref().map(std::path::Path::new),
            &config.sensitivity_effective(),
        );
        result_text = enforced.into_text();
    }

    // #673 — context-policy-pack redaction. Applies the active pack's
    // `[redaction]` patterns to outbound content before it reaches the model
    // (and before the out-of-band copy below). No-op when no pack is active,
    // so existing behavior is unchanged.
    if crate::core::policy::runtime::is_active() {
        let (redacted, hits) = policy_guard::redact_result(&result_text);
        if hits > 0 {
            tracing::debug!(redactions = hits, "context policy redaction applied");
            result_text = redacted;
        }
    }

    // #675 — inbound content filters (PII / classification / prompt-injection).
    // Runs at the same outbound chokepoint as redaction, before the archive /
    // compression below. A `block` decision replaces the content with a
    // refusal so it never reaches the model; `redact`/`warn` rewrite/annotate.
    // No-op unless the active pack enables a `[filters]` action.
    if let Some(active) = crate::core::policy::runtime::active()
        && active.filters.is_active()
    {
        let outcome = crate::core::input_filters::apply(&result_text, &active.filters);
        if outcome.blocked {
            let reason = outcome.block_reason.as_deref().unwrap_or("policy");
            tracing::warn!(tool = name, reason, "content blocked by input filter");
            policy_guard::audit_filter(name, &outcome.audit, true);
            result_text = format!(
                "[POLICY BLOCKED] Content withheld by the active context policy pack \
                     (input filter: {reason}). Adjust .lean-ctx/policy.toml to proceed."
            );
        } else {
            if !outcome.audit.is_empty() {
                tracing::debug!(tool = name, "input filters applied");
                policy_guard::audit_filter(name, &outcome.audit, false);
            }
            result_text = outcome.text;
            for warning in &outcome.warnings {
                result_text = format!("{result_text}\n\n[FILTER] {warning}");
            }
        }
    }

    // Out-of-band archive + optional context firewall for large tool outputs.
    // For firewallable tools (ctx_shell/ctx_execute/ctx_search/ctx_tree) whose output
    // exceeds the ephemeral threshold, the full (redacted) body is stored out-of-band
    // and the inline result is replaced by a compact digest + ctx_expand drilldown.
    let mut firewalled = false;
    let archive_hint = if minimal || is_raw_shell {
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
            match archive::store(name, &cmd, &to_store, Some(&session_id)) {
                Some(id) if crate::core::firewall::should_firewall(name, tokens, &config) => {
                    result_text = crate::core::firewall::summarize(&to_store, &id, name, tokens);
                    firewalled = true;
                    None
                }
                Some(id) => Some(archive::format_hint(&id, to_store.len(), tokens)),
                None => None,
            }
        } else {
            None
        }
    };

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

    if !firewalled
        && profile_hints.archive_hint()
        && let Some(hint) = archive_hint
    {
        result_text = format!("{result_text}\n{hint}");
    }

    if !is_raw_shell && let Some(ctx) = auto_context {
        let ctx_tokens = crate::core::tokens::count_tokens(&ctx);
        if ctx_tokens <= 400 {
            result_text = format!("{ctx}\n\n{result_text}");
        }
    }

    if let Some(warning) = throttle_warning {
        result_text = format!("{result_text}\n\n{warning}");
    }

    if let Some(bw) = budget_warning {
        result_text = format!("{result_text}\n\n{bw}");
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
            let _ = tokio::task::spawn_blocking(|| {
                if let Some(home) = dirs::home_dir() {
                    let _ = crate::rules_inject::inject_all_rules(&home);
                }
            })
            .await;
            result_text = format!(
                "{result_text}\n\n[RULES AUTO-UPDATED] Your lean-ctx rules were written by \
                     an older version and have been refreshed on disk. Start a new session to \
                     load them for full compatibility."
            );
        } else if !server
            .rules_tip_shown
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            let cfg = crate::core::config::Config::load();
            if !cfg.setup.should_inject_rules() {
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

    if name == "ctx_read" {
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
            let read_path = server
                .resolve_path_or_passthrough(&helpers::get_str(args, "path").unwrap_or_default())
                .await;
            let project_root = {
                let session = server.session.read().await;
                session.project_root.clone()
            };

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

            // Ledger update — fire-and-forget to avoid blocking concurrent reads.
            // Only real files belong in the context ledger (GL #512): a
            // ctx_read on "." or a directory returns an overview, not file
            // content, and must not appear in the pressure table as a file.
            if std::path::Path::new(&read_path).is_file() {
                let ledger_clone = server.ledger.clone();
                let session_clone = server.session.clone();
                let peer_clone = server.peer.clone();
                let read_path_owned = read_path.clone();
                let project_root_owned = project_root.clone();
                let mode_used =
                    helpers::get_str(args, "mode").unwrap_or_else(|| "auto".to_string());
                let out_tok = output_tokens as usize;
                let sent_tok = crate::core::tokens::count_tokens(&result_text);
                let wants_eviction = true;
                let wants_elicitation = profile_hints.elicitation_hint();
                tokio::spawn(async move {
                    let result = std::panic::AssertUnwindSafe(async {
                        let active_task = {
                            let session = session_clone.read().await;
                            session.task.as_ref().map(|t| t.description.clone())
                        };
                        let mut ledger = ledger_clone.write().await;
                        let overlay = crate::core::context_overlay::OverlayStore::load_project(
                            &std::path::PathBuf::from(project_root_owned.as_deref().unwrap_or(".")),
                        );
                        let gate_result = context_gate::post_dispatch_record_with_task(
                            &read_path_owned,
                            &mode_used,
                            out_tok,
                            sent_tok,
                            &mut ledger,
                            &overlay,
                            active_task.as_deref(),
                            project_root_owned.as_deref(),
                        );
                        drop(ledger);
                        if wants_eviction && let Some(hint) = &gate_result.eviction_hint {
                            tracing::debug!("deferred eviction hint: {hint}");
                        }
                        if wants_elicitation && let Some(hint) = &gate_result.elicitation_hint {
                            tracing::debug!("deferred elicitation hint: {hint}");
                        }
                        if let Some(hint) = &gate_result.prefetch_hint {
                            tracing::debug!("deferred FEP prefetch hint: {hint}");
                        }
                        if gate_result.resource_changed
                            && let Some(peer) = peer_clone.read().await.as_ref()
                        {
                            notifications::send_resource_updated(
                                peer,
                                notifications::RESOURCE_URI_SUMMARY,
                            )
                            .await;
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
                        tracing::error!("background post_dispatch panicked: {msg}");
                    }
                });
            }
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
        session.add_finding(finding.file.as_deref(), None, &finding.summary);
        let project_root = session.project_root.clone();
        drop(session);
        if let Some(ref root) = project_root {
            let f = finding.clone();
            let r = root.clone();
            std::thread::spawn(move || {
                crate::core::auto_capture::capture_finding(&r, &f);
            });
        }
    }
    if let Some(extra) = crate::core::auto_capture::extract_extra(name, &findings_source) {
        let session = server.session.read().await;
        let project_root = session.project_root.clone();
        drop(session);
        if let Some(ref root) = project_root {
            let e = extra.clone();
            let r = root.clone();
            std::thread::spawn(move || {
                crate::core::auto_capture::capture_finding(&r, &e);
            });
        }
    }

    {
        let tool_name = name.to_string();
        let summary = result_text.lines().next().unwrap_or("").to_string();
        // #520 opt-in debug log: a full per-call record (tool, args, result
        // preview, savings, wall time). Captured here and written off the hot
        // path in the existing journal thread; no-op unless `debug_log` is on.
        let dbg_args = args.cloned();
        let dbg_bytes = result_text.len();
        let dbg_saved = tool_saved_tokens;
        let dbg_elapsed = tool_start.elapsed();
        std::thread::spawn(move || {
            crate::core::journal::maybe_day_separator();
            crate::core::journal::log_tool_call(&tool_name, &summary);
            crate::core::debug_log::log_mcp_call(
                &tool_name,
                dbg_args.as_ref(),
                &summary,
                dbg_bytes,
                dbg_saved,
                dbg_elapsed,
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

    server
        .record_receipt_and_cost(
            name,
            args,
            action.as_deref(),
            &result_text,
            output_token_count,
        )
        .await;

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

    let skip_checkpoint = minimal
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

    if !skip_checkpoint
        && server.increment_and_check()
        && let Some(checkpoint) = server.auto_checkpoint().await
        && profile_hints.checkpoint_in_output()
        && crate::core::protocol::meta_visible()
    {
        // Stable header (#498): no interval interpolation — dynamic
        // text in repeated markers degrades provider prompt caching.
        let combined = format!("{result_text}\n\n--- AUTO CHECKPOINT ---\n{checkpoint}");
        return Ok(finalize_call_result(&combined, shell_outcome));
    }

    // #1020: tool-calls.log is now written on the dispatch path
    // (record_call_with_path / record_call_with_timing) with the real
    // original/saved/mode and the measured handler duration. The previous
    // zero-filled append here overwrote every row with `orig=0 saved=0 mode=-`.

    let current_count = server.call_count.load(std::sync::atomic::Ordering::Relaxed);
    if current_count > 0 && current_count.is_multiple_of(100) {
        std::thread::spawn(crate::cloud_sync::cloud_background_tasks);
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

    Ok(finalize_call_result(&result_text, shell_outcome))
}
