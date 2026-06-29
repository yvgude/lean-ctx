//! `LeanCtxServer::call_tool_guarded` — the guarded tool-dispatch path — and
//! root resolution. Split out of `server/mod.rs` to keep that module focused on
//! wiring. `use super::*` re-imports the parent aliases and sibling submodules.

#[allow(clippy::wildcard_imports)]
use super::*;

impl LeanCtxServer {
    pub(crate) async fn call_tool_guarded(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, ErrorData> {
        self.check_idle_expiry().await;
        self.resolve_roots_once().await;
        elicitation::increment_call();

        let original_name = request.name.as_ref().to_string();
        let (resolved_name, resolved_args) = if original_name == "ctx" {
            let sub = request
                .arguments
                .as_ref()
                .and_then(|a| a.get("tool"))
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string)
                .ok_or_else(|| {
                    ErrorData::invalid_params("'tool' is required for ctx meta-tool", None)
                })?;
            let tool_name = if sub.starts_with("ctx_") {
                sub
            } else {
                format!("ctx_{sub}")
            };
            let mut args = request.arguments.unwrap_or_default();
            args.remove("tool");
            (tool_name, Some(args))
        } else {
            (original_name, request.arguments)
        };
        let name = resolved_name.as_str();
        let args = resolved_args.as_ref();

        let role_check = role_guard::check_tool_access(name);
        if let Some(denied) = role_guard::into_call_tool_result(&role_check) {
            tracing::warn!(
                tool = name,
                role = %role_check.role_name,
                "Tool blocked by role policy"
            );
            return Ok(denied);
        }

        // #673 — context-policy-pack tool gating. Additive to the role guard:
        // a pack's `allow_tools`/`deny_tools` are enforced here. No-op (allow)
        // when no policy pack is active, so existing behavior is unchanged.
        let policy_check = policy_guard::check_tool_access(name);
        if let Some(denied) = policy_guard::into_call_tool_result(&policy_check) {
            tracing::warn!(
                tool = name,
                policy = ?policy_check.policy_name,
                "Tool blocked by context policy pack"
            );
            return Ok(denied);
        }

        // #676 — egress / output DLP on agent writes & actions. Inspect the
        // payload of write/action tools BEFORE dispatch so a forbidden write
        // never touches disk and a forbidden command never runs. Only the
        // agent's tool-driven egress is governed here (a human's own editor
        // writes never pass through this path). No-op unless the active pack has
        // an `[egress]` section.
        if let Some(active) = crate::core::policy::runtime::active()
            && active.egress.is_active()
        {
            let target = match name {
                "ctx_edit" => helpers::get_str(args, "new_string").map(|s| (s, "Write")),
                "ctx_shell" | "ctx_execute" => {
                    helpers::get_str(args, "command").map(|s| (s, "Action"))
                }
                _ => None,
            };
            if let Some((payload, kind)) = target {
                if let Some(reason) = active.egress.check_content(&payload, &active.redaction) {
                    tracing::warn!(tool = name, %reason, "agent egress blocked by policy");
                    policy_guard::audit_egress(name, &reason);
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "[POLICY BLOCKED] {kind} blocked by context policy pack egress rule \
                         ({reason}). Adjust .lean-ctx/policy.toml to proceed."
                    ))]));
                }
                if let Some(max) = active.egress.max_writes_per_min
                    && !crate::core::egress::check_rate(max)
                {
                    tracing::warn!(tool = name, max, "agent egress rate limit exceeded");
                    policy_guard::audit_egress(name, "rate-limit");
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "[POLICY BLOCKED] {kind} rate limit exceeded ({max}/min) by context \
                         policy pack. Slow agent writes/actions or adjust .lean-ctx/policy.toml."
                    ))]));
                }
            }
        }

        if name != "ctx_workflow" {
            let active = self.workflow.read().await.clone();
            if let Some(run) = active {
                if run.current == "done" || is_workflow_stale(&run) {
                    let mut wf = self.workflow.write().await;
                    *wf = None;
                    let _ = crate::core::workflow::clear_active();
                } else if !WORKFLOW_PASSTHROUGH_TOOLS.contains(&name)
                    && let Some(state) = run.spec.state(&run.current)
                    && let Some(allowed) = &state.allowed_tools
                {
                    let allowed_ok = allowed.iter().any(|t| t == name);
                    if !allowed_ok {
                        let mut shown = allowed.clone();
                        shown.sort();
                        shown.truncate(30);
                        return Ok(CallToolResult::success(vec![Content::text(format!(
                            "Tool '{name}' blocked by workflow '{}' (state: {}). Allowed: {}. Use ctx_workflow(action=\"stop\") to exit.",
                            run.spec.name,
                            run.current,
                            shown.join(", ")
                        ))]));
                    }
                }
            }
        }

        // #990: determine machine-readability *before* the once-per-session
        // decorations below. A machine-readable invocation (e.g. ctx_outline
        // format=json) must reach the client byte-exact and parseable, so every
        // prose decoration and terse compression is suppressed and the pure
        // pre-decoration body is restored at the end (see the `machine_readable`
        // guard near the end of this function). Computing it here — not after
        // dispatch — means such a call also never *consumes* a latched
        // once-per-session flag (auto-context briefing, rules tip) whose prose
        // we would then discard, so those surface on the next human-facing call.
        //
        // `ctx_call` is a meta-dispatcher: the contract belongs to its *inner*
        // tool + inner arguments, not to ctx_call itself. Unwrap one level so
        // JSON reached via the lazy `ctx_call` path (the default advertised
        // surface, where ctx_outline is not a top-level tool) is just as
        // byte-exact as a direct call. This also covers JSON error envelopes
        // from the early rate-limit path, which the first-call auto-context
        // briefing would otherwise corrupt.
        let (mr_name, mr_args): (
            Option<String>,
            Option<&serde_json::Map<String, serde_json::Value>>,
        ) = if name == "ctx_call" {
            (
                helpers::get_str(args, "name"),
                args.and_then(|m| m.get("arguments"))
                    .and_then(serde_json::Value::as_object),
            )
        } else {
            (Some(name.to_string()), args)
        };
        let machine_readable = mr_name
            .as_deref()
            .and_then(|n| self.registry.as_ref().and_then(|r| r.get_arc(n)))
            .is_some_and(|tool| tool.produces_machine_readable(mr_args));

        // Skip the session wake-up briefing for machine-readable calls: the
        // pre-hook latches `session_initialized` via compare-exchange, so calling
        // it here would burn the once-per-session slot for a briefing we then
        // throw away. Deferring keeps the briefing intact for the next call.
        let auto_context = if machine_readable {
            None
        } else {
            let task = {
                let session = self.session.read().await;
                session.task.as_ref().map(|t| t.description.clone())
            };
            let project_root = {
                let session = self.session.read().await;
                session.project_root.clone()
            };
            let cache_timeout =
                tokio::time::timeout(std::time::Duration::from_secs(5), self.cache.write()).await;
            if let Ok(mut cache) = cache_timeout {
                crate::tools::autonomy::session_lifecycle_pre_hook(
                    &self.autonomy,
                    name,
                    &mut cache,
                    task.as_deref(),
                    project_root.as_deref(),
                    CrpMode::effective(),
                )
            } else {
                tracing::warn!("pre-dispatch: cache write-lock timeout (5s), skipping autonomy");
                None
            }
        };

        let args_fp = args
            .map(|a| {
                crate::core::loop_detection::LoopDetector::fingerprint(&serde_json::Value::Object(
                    a.clone(),
                ))
            })
            .unwrap_or_default();
        let throttle_result = {
            let fp = &args_fp;
            let detector_timeout = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                self.loop_detector.write(),
            )
            .await;
            if let Ok(mut detector) = detector_timeout {
                let is_search = crate::core::loop_detection::LoopDetector::is_search_tool(name);
                let is_search_shell = name == "ctx_shell" && {
                    let cmd = args
                        .as_ref()
                        .and_then(|a| a.get("command"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    crate::core::loop_detection::LoopDetector::is_search_shell_command(cmd)
                };

                if is_search || is_search_shell {
                    let search_pattern = args.and_then(|a| {
                        a.get("pattern")
                            .or_else(|| a.get("query"))
                            .and_then(|v| v.as_str())
                    });
                    let shell_pattern = if is_search_shell {
                        args.and_then(|a| a.get("command"))
                            .and_then(|v| v.as_str())
                            .and_then(helpers::extract_search_pattern_from_command)
                    } else {
                        None
                    };
                    let pat = search_pattern.or(shell_pattern.as_deref());
                    detector.record_search(name, fp, pat)
                } else {
                    detector.record_call(name, fp)
                }
            } else {
                tracing::warn!("pre-dispatch: loop_detector write-lock timeout (3s), skipping");
                crate::core::loop_detection::ThrottleResult::default()
            }
        };

        if throttle_result.level == crate::core::loop_detection::ThrottleLevel::Blocked {
            let msg = throttle_result.message.unwrap_or_default();
            return Ok(CallToolResult::success(vec![Content::text(msg)]));
        }

        let throttle_warning =
            if throttle_result.level == crate::core::loop_detection::ThrottleLevel::Reduced {
                throttle_result.message.clone()
            } else {
                None
            };

        let config = crate::core::config::Config::load_arc();
        let minimal = config.minimal_overhead_effective();

        // IDE permission inheritance: when enabled, mirror the host IDE's
        // bash/read/edit/grep permission rules onto the matching lean-ctx tool so
        // e.g. `ctx_shell` honors a `rm *: ask` rule instead of bypassing it.
        // Gated on the cheap effective() check so the default (off) pays no lock
        // cost on the hot path.
        if config.permission_inheritance_effective()
            == crate::core::config::PermissionInheritance::On
        {
            let client_name = self.client_name.read().await.clone();
            let project_root = self.session.read().await.project_root.clone();
            let perm = permission_inheritance::check(
                &client_name,
                name,
                args,
                project_root.as_deref(),
                &config,
            );
            if let Some(blocked) = permission_inheritance::into_call_tool_result(&perm) {
                tracing::warn!(tool = name, "held back by IDE permission inheritance");
                return Ok(blocked);
            }
        }

        if let Some(msg) = post_process::budget_exhausted_message(name) {
            tracing::warn!(tool = name, "{msg}");
            return Ok(CallToolResult::success(vec![Content::text(msg)]));
        }

        if is_shell_tool_name(name) {
            crate::core::budget_tracker::BudgetTracker::global().record_shell();
        }

        let tool_start = std::time::Instant::now();
        let (mut result_text, tool_saved_tokens, shell_outcome) =
            match self.dispatch_tool(name, args, minimal).await {
                Ok(triple) => triple,
                Err(e) => {
                    if let Ok(mut detector) = tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        self.loop_detector.write(),
                    )
                    .await
                    {
                        detector.record_error_outcome(name, &args_fp);
                    }
                    crate::core::debug_log::log_mcp_error(name, args, &format!("{e:?}"));
                    return Err(e);
                }
            };

        let is_raw_shell = name == "ctx_shell" && {
            let arg_raw = helpers::get_bool(args, "raw").unwrap_or(false);
            let arg_bypass = helpers::get_bool(args, "bypass").unwrap_or(false);
            arg_raw
                || arg_bypass
                || std::env::var("LEAN_CTX_DISABLED").is_ok()
                || std::env::var("LEAN_CTX_RAW").is_ok()
        };

        let pre_terse_len = result_text.len();
        let output_tokens = {
            let tokens = crate::core::tokens::count_tokens(&result_text) as u64;
            crate::core::budget_tracker::BudgetTracker::global().record_tokens(tokens);
            tokens
        };

        crate::core::anomaly::record_metric("tokens_per_call", output_tokens as f64);

        // Context IR: record lineage for every tool call.
        if let Some(ref ir) = self.context_ir {
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
            let mut detector = self.loop_detector.write().await;
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
        // out-of-band copy. No-op unless `sensitivity.enabled` (default off).
        {
            let path_hint = helpers::get_str(args, "path");
            let enforced = crate::core::sensitivity::enforce_text(
                std::mem::take(&mut result_text),
                path_hint.as_deref().map(std::path::Path::new),
                &config.sensitivity,
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
                let session_id = self.session.read().await.id.clone();
                let to_store = crate::core::redaction::redact_text_if_enabled(&result_text);
                let tokens = crate::core::tokens::count_tokens(&to_store);
                match archive::store(name, &cmd, &to_store, Some(&session_id)) {
                    Some(id) if crate::core::firewall::should_firewall(name, tokens, &config) => {
                        result_text =
                            crate::core::firewall::summarize(&to_store, &id, name, tokens);
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
            result_text =
                post_process::compress_terse(result_text, name, args, &config, is_raw_shell);
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
            && !self
                .rules_stale_checked
                .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            let client = self.client_name.read().await.clone();
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
            } else if !self
                .rules_tip_shown
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                let cfg = crate::core::config::Config::load();
                if !cfg.setup.should_inject_rules() {
                    result_text = format!(
                        "{result_text}\n\n\
                         --- tip: run 'lean-ctx setup --inject-rules' for optimal AI integration ---"
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
                let cache_clone = self.cache.clone();
                let autonomy_clone = self.autonomy.clone();
                let name_owned = name.to_string();
                tokio::spawn(async move {
                    let result = std::panic::AssertUnwindSafe(async {
                        let mut cache = cache_clone.write().await;
                        crate::tools::autonomy::maybe_auto_dedup(
                            &autonomy_clone,
                            &mut cache,
                            &name_owned,
                        );
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
                let read_path = self
                    .resolve_path_or_passthrough(
                        &helpers::get_str(args, "path").unwrap_or_default(),
                    )
                    .await;
                let project_root = {
                    let session = self.session.read().await;
                    session.project_root.clone()
                };

                // Bounded cache lock for enrichment — degrade gracefully under contention
                let enrich_timeout =
                    tokio::time::timeout(std::time::Duration::from_secs(3), self.cache.write())
                        .await;
                if let Ok(mut cache) = enrich_timeout {
                    let enrich = crate::tools::autonomy::enrich_after_read(
                        &self.autonomy,
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
                    crate::tools::autonomy::maybe_auto_dedup(&self.autonomy, &mut cache, name);
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
                    let ledger_clone = self.ledger.clone();
                    let session_clone = self.session.clone();
                    let peer_clone = self.peer.clone();
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
                                &std::path::PathBuf::from(
                                    project_root_owned.as_deref().unwrap_or("."),
                                ),
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
                let calls = self.tool_calls.read().await;
                let last_original = calls.last().map_or(0, |c| c.original_tokens);
                drop(calls);
                let pre_hint_tokens = crate::core::tokens::count_tokens(&result_text);
                if let Some(hint) = crate::tools::autonomy::shell_efficiency_hint(
                    &self.autonomy,
                    &cmd,
                    last_original,
                    pre_hint_tokens,
                ) {
                    result_text = format!("{result_text}\n{hint}");
                }
            }
        }

        if !minimal && !is_raw_shell {
            if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
                let session = self.session.read().await;
                bypass_hint::set_session_id(&session.id);
                drop(session);
                if let Some(hint) = bypass_hint::check(&data_dir) {
                    result_text = format!("{result_text}\n{hint}");
                }
            }
            bypass_hint::record_lctx_call();
        }

        if let Some(finding) = crate::core::auto_findings::extract(name, &result_text) {
            let mut session = self.session.write().await;
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
        if let Some(extra) = crate::core::auto_capture::extract_extra(name, &result_text) {
            let session = self.session.read().await;
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
        if self.session_mode == crate::tools::SessionMode::Shared
            && let Some(ref rt) = self.context_os
        {
            let latest = rt.bus.latest_id(&self.workspace_id, &self.channel_id);
            let cursor = self
                .last_seen_event_id
                .load(std::sync::atomic::Ordering::Relaxed);
            if cursor > 0 && latest - cursor > K_STALENESS_BOUND {
                let gap = latest - cursor;
                result_text = format!(
                    "[CONTEXT STALE] {gap} events happened since your last read. \
                         Use ctx_session(action=\"status\") to sync.\n\n{result_text}"
                );
            }
            self.last_seen_event_id
                .store(latest, std::sync::atomic::Ordering::Relaxed);
        }

        self.record_receipt_and_cost(
            name,
            args,
            action.as_deref(),
            &result_text,
            output_token_count,
        )
        .await;

        // Context Bus: conflict detection for knowledge writes in shared mode.
        if self.session_mode == crate::tools::SessionMode::Shared
            && name == "ctx_knowledge"
            && action.as_deref() == Some("remember")
            && let Some(ref rt) = self.context_os
        {
            let my_agent = self.agent_id.read().await.clone();
            let category = helpers::get_str(args, "category");
            let key = helpers::get_str(args, "key");
            if let (Some(cat), Some(k)) = (&category, &key) {
                let recent = rt.bus.recent_by_kind(
                    &self.workspace_id,
                    &self.channel_id,
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

        self.persist_shared_context_os(name, action.as_deref(), args)
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
            && self.increment_and_check()
            && let Some(checkpoint) = self.auto_checkpoint().await
            && profile_hints.checkpoint_in_output()
            && crate::core::protocol::meta_visible()
        {
            // Stable header (#498): no interval interpolation — dynamic
            // text in repeated markers degrades provider prompt caching.
            let combined = format!("{result_text}\n\n--- AUTO CHECKPOINT ---\n{checkpoint}");
            return Ok(finalize_call_result(combined, shell_outcome));
        }

        // #1020: tool-calls.log is now written on the dispatch path
        // (record_call_with_path / record_call_with_timing) with the real
        // original/saved/mode and the measured handler duration. The previous
        // zero-filled append here overwrote every row with `orig=0 saved=0 mode=-`.

        let current_count = self.call_count.load(std::sync::atomic::Ordering::Relaxed);
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
            if let Some(root) = self.session.read().await.project_root.clone() {
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

        Ok(finalize_call_result(result_text, shell_outcome))
    }

    /// Resolve project root from MCP client roots (once per session).
    /// Called on the first tool call. If the client supports `roots/list`,
    /// we query it and pick the best root with project markers.
    async fn resolve_roots_once(&self) {
        use std::sync::atomic::Ordering;
        if !self.has_client_roots.load(Ordering::Relaxed) {
            return;
        }
        if self.roots_resolved.swap(true, Ordering::Relaxed) {
            return;
        }
        let peer_guard = self.peer.read().await;
        let Some(peer) = peer_guard.as_ref() else {
            return;
        };
        let list_result = match peer.list_roots().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("roots/list failed: {e}");
                return;
            }
        };
        drop(peer_guard);

        let uris: Vec<String> = list_result.roots.iter().map(|r| r.uri.clone()).collect();
        let validated_paths = roots::valid_dir_paths_from_uris(&uris);
        let Some(new_root) = roots::best_root_from_uris(&uris) else {
            return;
        };
        // Defense-in-depth: never adopt a broad/unsafe root (HOME, `/`, agent
        // sandbox dirs) even if the client reports it — it would pollute the
        // session and resolve relative paths against the wrong tree.
        if crate::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(&new_root)) {
            tracing::warn!("MCP roots: ignoring unsafe project root {new_root}");
            return;
        }

        let mut session = self.session.write().await;
        let old_root = session.project_root.clone();

        let other_roots: Vec<String> = validated_paths
            .iter()
            .filter(|p| p.as_str() != new_root)
            .cloned()
            .collect();
        if !other_roots.is_empty() {
            session.extra_roots = other_roots;
            tracing::info!(
                "MCP roots: {} extra root(s) registered",
                session.extra_roots.len()
            );
        }

        if old_root.as_deref() == Some(&new_root) {
            let _ = session.save();
            return;
        }
        tracing::info!(
            "MCP roots: switching project root from {:?} to {new_root}",
            old_root
        );
        if let Some(existing) =
            crate::core::session::SessionState::load_latest_for_project_root(&new_root)
        {
            *session = existing;
            session.extra_roots = validated_paths
                .iter()
                .filter(|p| p.as_str() != new_root)
                .cloned()
                .collect();
        }
        session.project_root = Some(new_root);
        let _ = session.save();
        drop(session);
        // Indices warm lazily on first use of a tool that needs them (#152) —
        // the dispatch path for this very call handles it via
        // `index_orchestrator::ensure_warm_for_tool`, so no eager scan here.
    }
}

/// Build the final `CallToolResult`, surfacing shell failures in MCP metadata
/// (GitHub #389): a non-zero exit or a blocked command sets `isError: true`
/// and a `structuredContent` payload (`{"exitCode": N}` / `{"blocked": true}`),
/// so clients no longer have to regex-parse the `[exit:N]` text footer. The
/// text content is identical in both cases — only the metadata changes.
fn finalize_call_result(
    result_text: String,
    shell_outcome: Option<crate::server::tool_trait::ShellOutcome>,
) -> CallToolResult {
    let mut result = CallToolResult::success(vec![Content::text(result_text)]);
    if let Some(outcome) = shell_outcome {
        if outcome.is_error() {
            result.is_error = Some(true);
        }
        if let Some(structured) = outcome.structured() {
            result.structured_content = Some(structured);
        }
    }
    result
}

#[cfg(test)]
mod shell_outcome_tests {
    use super::*;
    use crate::server::tool_trait::ShellOutcome;

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn success_exit_is_not_an_error() {
        let r = finalize_call_result("ok".into(), Some(ShellOutcome::Exit(0)));
        assert_ne!(r.is_error, Some(true), "exit 0 must not set isError");
        assert!(
            r.structured_content.is_none(),
            "happy path stays token-neutral: no structuredContent on exit 0"
        );
        assert_eq!(text_of(&r), "ok");
    }

    #[test]
    fn nonzero_exit_sets_is_error_and_structured_exit_code() {
        let r = finalize_call_result("boom\n[exit:1]".into(), Some(ShellOutcome::Exit(1)));
        assert_eq!(
            r.is_error,
            Some(true),
            "non-zero exit must set isError (#389)"
        );
        assert_eq!(
            r.structured_content,
            Some(serde_json::json!({ "exitCode": 1 })),
            "guards must be able to read exitCode without text parsing"
        );
        assert_eq!(
            text_of(&r),
            "boom\n[exit:1]",
            "text content must be preserved"
        );
    }

    #[test]
    fn negative_exit_codes_are_reported() {
        // Signal terminations are mapped to negative/128+n codes by execute();
        // whatever the value, non-zero must surface as an error.
        let r = finalize_call_result("killed".into(), Some(ShellOutcome::Exit(-1)));
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            r.structured_content,
            Some(serde_json::json!({ "exitCode": -1 }))
        );
    }

    #[test]
    fn blocked_command_sets_is_error_and_blocked_marker() {
        let r = finalize_call_result("[BLOCKED] nope".into(), Some(ShellOutcome::Blocked));
        assert_eq!(
            r.is_error,
            Some(true),
            "blocked commands never ran — that is a failure"
        );
        assert_eq!(
            r.structured_content,
            Some(serde_json::json!({ "blocked": true }))
        );
    }

    #[test]
    fn non_shell_tools_are_unaffected() {
        let r = finalize_call_result("file contents".into(), None);
        assert_ne!(r.is_error, Some(true));
        assert!(r.structured_content.is_none());
    }
}
