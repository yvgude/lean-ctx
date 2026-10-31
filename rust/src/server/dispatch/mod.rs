// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData;
use serde_json::Value;

use crate::server::helpers::get_str;
use crate::server::tool_trait::{McpTool, ShellOutcome, ToolContext, ToolOutput};
use crate::tools::LeanCtxServer;
use rmcp::model::ContentBlock;

impl LeanCtxServer {
    /// Returns (output_text, saved_tokens, shell_outcome, content_blocks).
    /// saved_tokens > 0 indicates the tool already applied internal compression
    /// (shell engine, cache deltas, etc.). shell_outcome is `Some` for
    /// shell-executing tools so the caller can populate MCP error metadata
    /// (#389). content_blocks carries image/binary MCP blocks when present.
    pub(super) async fn dispatch_tool(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<
        (
            String,
            usize,
            Option<ShellOutcome>,
            Option<Vec<ContentBlock>>,
        ),
        ErrorData,
    > {
        fn format_rate_limited(
            tool: &str,
            agent_id: &str,
            retry_after_ms: u64,
            args: Option<&serde_json::Map<String, Value>>,
        ) -> String {
            let as_json = get_str(args, "format").as_deref() == Some("json");
            if as_json {
                serde_json::json!({
                    "error": "rate_limited",
                    "tool": tool,
                    "agent_id": agent_id,
                    "retry_after_ms": retry_after_ms,
                })
                .to_string()
            } else {
                format!("[RATE LIMITED] tool={tool} retry_after_ms={retry_after_ms}")
            }
        }

        // A host replaces a native tool's output with a host hook's success
        // body, so a gate refusal on such a call must be an error: then the host
        // keeps the original output instead of a block message (#2089).
        let host_hook = crate::server::dynamic_tools::targets_internal_host_tool(name, args);
        let refuse = |text: String| {
            if host_hook {
                Err(ErrorData::invalid_request(text, None))
            } else {
                Ok((text, 0, None, None))
            }
        };

        // Hive tools follow the Hive release switch: off, every path (direct, ctx_call, ctx)
        // answers one line before any rate or capability gate.
        if let Some(line) = crate::server::dynamic_tools::withheld_hive_tool_refusal(name, args) {
            return Ok((line.to_owned(), 0, None, None));
        }

        let agent_id = self
            .agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        {
            if let crate::core::a2a::rate_limiter::RateLimitResult::Limited { retry_after_ms } =
                crate::core::a2a::rate_limiter::check_rate_limit(&agent_id, name)
            {
                return refuse(format_rate_limited(name, &agent_id, retry_after_ms, args));
            }
        }

        if name.starts_with("ctx_") {
            crate::server::tool_visibility::mark_auto_ctx_tool_used();
        }
        // K13: a tool call of a host family no enforcing hook regulates.
        if crate::core::hive::metrics_v2::signals::recording() {
            let client = self.client_name.read().await.clone();
            crate::core::hive::metrics_v2::signals::host_call(&client);
        }
        // Background semantic passes yield to tool calls in flight.
        let _activity = crate::core::graph_enricher::ToolActivity::begin();

        match name {
            "ctx_call" => {
                let (inner, arg_map) = crate::tools::registered::ctx_call::resolve_inner(args)?;

                if let crate::core::a2a::rate_limiter::RateLimitResult::Limited { retry_after_ms } =
                    crate::core::a2a::rate_limiter::check_rate_limit(&agent_id, &inner)
                {
                    return refuse(format_rate_limited(
                        &inner,
                        &agent_id,
                        retry_after_ms,
                        arg_map.as_ref(),
                    ));
                }

                let inner_role_check = crate::server::role_guard::check_tool_access(&inner);
                if let Some(denied) =
                    crate::server::role_guard::into_call_tool_result(&inner_role_check)
                {
                    let msg = denied
                        .content
                        .first()
                        .and_then(|c| c.as_text())
                        .map_or_else(|| "Blocked by role policy".to_string(), |t| t.text.clone());
                    return refuse(msg);
                }

                // The decision is `core::workflow::tool_gate`, shared with the
                // direct gate (`call_tool/policy.rs::guard_workflow`).
                let active = self.workflow.read().await.clone();
                if let Some(run) = active {
                    use crate::core::workflow::ToolGate;
                    match crate::core::workflow::tool_gate(
                        &run,
                        &inner,
                        super::WORKFLOW_PASSTHROUGH_TOOLS,
                    ) {
                        ToolGate::Pass => {}
                        ToolGate::Expired => {
                            let mut wf = self.workflow.write().await;
                            *wf = None;
                            let _ = crate::core::workflow::clear_active();
                        }
                        ToolGate::Refuse(reason) => return refuse(reason),
                    }
                }

                let result = self
                    .dispatch_inner(&inner, arg_map.as_ref(), minimal)
                    .await?;
                // Inner tool is already recorded by dispatch_inner with correct tokens.
                Ok(result)
            }
            _ => self.dispatch_inner(name, args, minimal).await,
        }
    }

    /// Dispatches a single tool via the trait-based registry.
    /// Returns (output_text, saved_tokens, shell_outcome).
    async fn dispatch_inner(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<
        (
            String,
            usize,
            Option<ShellOutcome>,
            Option<Vec<ContentBlock>>,
        ),
        ErrorData,
    > {
        // #454: when the user prefers their host's native editor, lean-ctx edit
        // operations are fully disabled — refused here so neither a direct call
        // nor `ctx_call` can reach them (list_tools already hides them).
        if crate::core::config::Config::load().edit_tool_blocked(name) {
            return Ok((
                format!(
                    "[disabled] '{name}' is turned off (prefer_native_editor): use your editor's \
                     built-in edit tool. Re-enable with `lean-ctx config set prefer_native_editor false`."
                ),
                0,
                None,
                None,
            ));
        }

        // Hive tools follow the Hive release switch: off, they never run.
        if crate::server::dynamic_tools::is_withheld_hive_tool(name) {
            return Ok((
                crate::core::hive::TOOL_NOT_AVAILABLE.to_owned(),
                0,
                None,
                None,
            ));
        }

        // Hive claims belong to the calling agent's session (C8): a shared
        // server serves several clients under one process and agent
        // identity, so it cannot bind a claim to its caller.
        if name == "ctx_tasks" && self.session_mode == crate::tools::SessionMode::Shared {
            return Ok((
                "[unavailable] ctx_tasks works only in a personal MCP session: a shared \
                 server cannot bind a Hive claim to the client that calls it."
                    .to_owned(),
                0,
                None,
                None,
            ));
        }

        if let Some(tool) = self.registry.as_ref().and_then(|r| r.get_arc(name)) {
            let empty = serde_json::Map::new();
            let args_map = args.unwrap_or(&empty);
            let aliased = canonicalize_arg_aliases(tool.as_ref(), args_map);
            let args_map = aliased.as_ref().unwrap_or(args_map);
            let project_root = {
                let session = self.session.read().await;
                session.project_root.clone().unwrap_or_default()
            };

            // Lazy, demand-driven index warming (#152): only tools that actually
            // need a prebuilt index trigger a (background, once-per-root) scan.
            // The first heavy pre-warm also warms any configured extra roots once.
            let authority = crate::core::workspace_authority::current();
            // Under a Workspace only its own roots are indexed (#2120); an
            // unusable one grants no root, so nothing is.
            let may_index = crate::core::workspace_authority::root_authorized(
                std::path::Path::new(&project_root),
            );
            if may_index
                && !project_root.is_empty()
                && crate::core::index_orchestrator::ensure_warm_for_tool(&project_root, name)
            {
                let extra_roots = match authority.snapshot() {
                    Some(snapshot) => snapshot.additional_root_strings(),
                    None => self.session.read().await.extra_roots.clone(),
                };
                if !extra_roots.is_empty() {
                    let primary = project_root.clone();
                    std::thread::spawn(move || {
                        crate::core::index_orchestrator::ensure_extra_roots_background(
                            &primary,
                            &extra_roots,
                        );
                    });
                }
            }

            let mut resolved_paths = std::collections::HashMap::new();
            let mut path_errors: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for key in PATH_LIKE_KEYS {
                if let Some(val) = args_map.get(*key) {
                    if let Some(raw) = val.as_str() {
                        match self.resolve_path_refusing(raw).await {
                            Ok(resolved) => {
                                if !["path", "project_root", "root"].contains(key) {
                                    tracing::trace!(
                                        "[pathjail] resolved non-standard path key '{key}': {raw} -> {resolved}"
                                    );
                                }
                                resolved_paths.insert(key.to_string(), resolved);
                            }
                            Err(refusal) => {
                                tracing::debug!(
                                    "[dispatch] path resolution failed for '{key}' = '{raw}': {}",
                                    refusal.message
                                );
                                // Recorded once, here, with the calling tool.
                                self.record_path_refusal(name, &refusal).await;
                                path_errors.insert(key.to_string(), refusal.message);
                            }
                        }
                    } else {
                        let type_name = match val {
                            serde_json::Value::Number(_) => "number",
                            serde_json::Value::Bool(_) => "boolean",
                            serde_json::Value::Array(_) => "array",
                            serde_json::Value::Object(_) => "object",
                            serde_json::Value::Null => "null",
                            serde_json::Value::String(_) => unreachable!(),
                        };
                        path_errors.insert(
                            key.to_string(),
                            format!("{key} must be a string, got {type_name}"),
                        );
                    }
                }
            }

            let crp_mode = crate::tools::CrpMode::effective();
            // One snapshot after path resolution, which may have corrected a
            // stale root: the handler's root and its writable roots agree
            // (AP04-G6). The home jail scope opens other projects for reading
            // only; these roots stay writable for this call (pathjail_scope).
            let (project_root, extra_roots) = {
                let session = self.session.read().await;
                (
                    session.project_root.clone().unwrap_or_default(),
                    session.extra_roots.clone(),
                )
            };
            // Under a Workspace the call's roots are the snapshot's (#2120),
            // whatever the session carries; everything that derives roots from
            // the context (admitted sources, write roots, shell capture)
            // follows.
            let (project_root, extra_roots) = match authority.snapshot() {
                Some(snapshot) => {
                    let root = snapshot.project_root_for(&project_root);
                    let extra = snapshot
                        .granting_roots()
                        .map(|(path, _)| path.to_string_lossy().into_owned())
                        .filter(|path| *path != root)
                        .collect();
                    (root, extra)
                }
                // An unusable Workspace leaves the call without any root.
                None if authority.is_active() => (String::new(), Vec::new()),
                None => (project_root, extra_roots),
            };
            let ctx = crate::server::tool_trait::ToolContext {
                project_root,
                extra_roots,
                minimal,
                resolved_paths,
                crp_mode,
                cache: Some(self.cache.clone()),
                session: Some(self.session.instance()),
                tool_calls: Some(self.tool_calls.clone()),
                agent_id: Some(self.agent_id.clone()),
                workflow: Some(self.workflow.clone()),
                ledger: Some(self.ledger.clone()),
                client_name: Some(self.client_name.clone()),
                client_role: None,
                shell_access: None,
                pipeline_stats: Some(self.pipeline_stats.clone()),
                call_count: Some(self.call_count.clone()),
                autonomy: Some(self.autonomy.clone()),
                path_errors,
                bm25_cache: Some(self.bm25_cache.clone()),
                progress_sender: Some(self.progress_sender.clone()),
                // #1781: the request's cancellation token, scoped per request by
                // `call_tool`. `try_with` yields `None` outside such a scope —
                // unit tests and internal dispatch paths — which leaves their
                // behaviour exactly as it was.
                cancel: crate::server::tool_trait::REQUEST_CT
                    .try_with(tokio_util::sync::CancellationToken::clone)
                    .ok(),
            };
            // Run the (synchronous) handler on the dedicated blocking pool under
            // a watchdog deadline (#271). `block_in_place` would pin one of the
            // few core workers and — being synchronous — cannot be interrupted
            // by `tokio::time::timeout` from the same task, so a hung handler
            // would silently swallow the JSON-RPC response and the MCP client
            // would crash with "Cannot read properties of undefined (reading
            // 'invoke')". `spawn_blocking` keeps the core workers free and lets
            // the watchdog always return a response.
            let provider_snapshot =
                name == "ctx_provider" && crate::tools::ctx_provider::is_snapshot_request(args_map);
            let handler_started = std::time::Instant::now();
            // Pair each handler's origin with its own output, including nested calls.
            let (output, observed_origin) = crate::core::archive::authority::capture(
                self.run_tool_handler(name, tool, args_map, ctx),
            )
            .await;
            let output = output?;
            // The tool's tokenizer measurement of its sources, for the receipt.
            if let Some(admissions) = crate::core::context_admission::capture::current() {
                admissions.set_original_tokens(output.original_tokens as u64);
            }
            let file_output = name == "ctx_execute"
                && args_map.get("action").and_then(Value::as_str) == Some("file")
                && matches!(output.shell_outcome, Some(ShellOutcome::Exit(0)));
            // Hand every observed source — and an incomplete observation as
            // such — to the outer pipeline capture: it decides, from the
            // tool's own declaration, whether the set may authorize reuse
            // (replay, response cache, source-bound archive). Nested handlers
            // join the same outer set.
            if let Some(origin) = &observed_origin {
                origin.clone().publish();
            }
            // The reference store below keeps its narrower rule: only the
            // admitted file handler's output is bound there.
            let output_origin = observed_origin.filter(|_| file_output);
            let handler_ms = handler_started.elapsed().as_millis() as u64;

            // Image/binary content blocks bypass all text processing.
            if output.content_blocks.is_some() {
                if let Some(ref path) = output.path {
                    self.record_call_with_path(
                        name,
                        0,
                        0,
                        Some("image".to_string()),
                        Some(path),
                        handler_ms,
                    )
                    .await;
                } else {
                    self.record_call_with_timing(name, 0, 0, Some("image".to_string()), handler_ms)
                        .await;
                }
                return Ok((
                    String::new(),
                    0,
                    output.shell_outcome,
                    output.content_blocks,
                ));
            }

            let config_changed =
                super::tools_config_watch::has_changed(&self.last_tools_config_hash);
            if (output.changed || config_changed)
                && let Some(peer) = self.peer.read().await.as_ref()
            {
                if config_changed {
                    tracing::info!(
                        "Tool-config changed (profile/enabled/disabled) — sending tools/list_changed"
                    );
                }
                super::notifications::send_tools_list_changed(peer).await;
            }

            let headers_only =
                crate::core::config::ResponseVerbosity::effective().is_headers_only();
            let header_line = if headers_only && !provider_snapshot {
                Some(output.to_header_line(name))
            } else {
                None
            };

            let output_token_estimate = crate::core::tokens::count_tokens(&output.text) as u32;

            if let Some(ref path) = output.path {
                {
                    // Skip ledger record for ctx_read — it's recorded in post_dispatch
                    // with correct final token counts after terse compression.
                    if name != "ctx_read" {
                        let sent_tokens = if output.original_tokens > 0 {
                            output.original_tokens.saturating_sub(output.saved_tokens)
                        } else {
                            crate::core::tokens::count_tokens(&output.text)
                        };
                        let orig = if output.original_tokens > 0 {
                            output.original_tokens
                        } else {
                            sent_tokens
                        };
                        let mode_str = output.mode.as_deref().unwrap_or("full");
                        let mut ledger = self.ledger.write().await;
                        ledger.record(path, mode_str, orig, sent_tokens);
                        ledger.save_debounced();
                    }
                }
                self.record_call_with_path(
                    name,
                    output.original_tokens,
                    output.saved_tokens,
                    output.mode,
                    Some(path),
                    handler_ms,
                )
                .await;
            } else {
                self.record_call_with_timing(
                    name,
                    output.original_tokens,
                    output.saved_tokens,
                    output.mode,
                    handler_ms,
                )
                .await;
            }

            let agent_id = self
                .agent_id
                .read()
                .await
                .clone()
                .unwrap_or_else(|| "unknown".into());
            let role = crate::core::roles::active_role_name();
            {
                let input_hash = crate::core::audit_trail::hash_input(args_map);
                crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
                    agent_id: agent_id.clone(),
                    tool: name.to_string(),
                    action: None,
                    input_hash,
                    output_tokens: output_token_estimate,
                    role: role.clone(),
                    event_type: crate::core::audit_trail::AuditEventType::ToolCall,
                });
            }

            let saved = output.saved_tokens;
            let raw_text = header_line.unwrap_or(output.text);
            let final_text = if provider_snapshot {
                raw_text
            } else {
                sanitized_tool_text(name, raw_text)
            };

            // Context immune system: scan for prompt-injection patterns in tool output,
            // read as the kind of content its source file is (code, logs, prose).
            let output_kind = crate::core::output_sanitizer::ContentKind::of(
                &final_text,
                output.path.as_deref().map(std::path::Path::new),
            );
            let injection_signals =
                crate::core::output_sanitizer::detect_injection_as(&final_text, output_kind);
            // Sources the gateway already flagged are counted by their receipt.
            let gateway_flagged = crate::core::context_admission::capture::current()
                .is_some_and(|admissions| admissions.flagged_injection());
            if !injection_signals.is_empty() && !gateway_flagged {
                tracing::warn!(
                    tool = name,
                    lines = injection_signals.len(),
                    "prompt-injection patterns detected in tool output"
                );
                // Flagged, not neutralized: the output still reaches the agent.
                // One flag per output object, recorded once per process.
                let flag = crate::core::security_events::InjectionFlag {
                    object_sha256: {
                        use sha2::Digest as _;
                        crate::core::agent_identity::hex_encode(&sha2::Sha256::digest(
                            final_text.as_bytes(),
                        ))
                    },
                    patterns: crate::core::output_sanitizer::InjectionCategory::mask(
                        &injection_signals,
                    ),
                };
                if crate::core::security_events::record_injections(name, &agent_id, &[flag]) > 0 {
                    self.publish_value_snapshot().await;
                }
            }

            let reference_enabled = std::env::var("LEAN_CTX_REFERENCE_RESULTS").map_or_else(
                |_| crate::core::config::Config::load().reference_results,
                |v| v == "1" || v == "true",
            );

            // An explicit file read must always return its content — never a
            // stored-reference stub — so the agent can edit against the lines. The
            // firewall already exempts reads; the reference-results path honours the
            // same rule via `is_protected_read` (otherwise enabling reference_results
            // silently turns `ctx_read` into an un-editable "Output stored …" preview).
            if reference_enabled
                && !provider_snapshot
                && !crate::core::firewall::is_protected_read(name)
                && !output.shell_outcome.as_ref().is_some_and(
                    crate::server::tool_trait::ShellOutcome::is_terminal_background_status,
                )
                && final_text.len() > REFERENCE_THRESHOLD
                && let Some(ref_id) = super::reference_store::store_with_authority(
                    &final_text,
                    output_origin.as_ref(),
                )
            {
                let mut preview_end = final_text.len().min(200);
                while preview_end > 0 && !final_text.is_char_boundary(preview_end) {
                    preview_end -= 1;
                }
                let resolve = if output_origin.is_some() {
                    format!("ctx_expand(id=\"{ref_id}\")")
                } else {
                    format!("/v1/references/{ref_id}")
                };
                let summary = format!(
                    "[Reference: {ref_id}] Output stored ({} chars, ~{} tokens). Resolve: {resolve}\nPreview: {}...",
                    final_text.len(),
                    final_text.len() / 4,
                    &final_text[..preview_end]
                );
                // The outcome must survive the reference-store substitution —
                // a failed shell command stays a failure even when its output
                // is delivered out-of-band (#389).
                return Ok((summary, saved, output.shell_outcome, None));
            }

            return Ok((final_text, saved, output.shell_outcome, None));
        }

        // A removed tool names its successor; an unknown one (#712) gets the
        // closest registered name so a typo (`ctx_raed`) costs a hint, not a
        // blind retry.
        if let Some(message) = crate::server::dynamic_tools::removed_tool_message(name) {
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                message,
                None,
            ));
        }
        let suggestion = self
            .registry
            .as_ref()
            .and_then(|r| {
                let names = r
                    .names()
                    .into_iter()
                    .filter(|n| !crate::server::dynamic_tools::is_withheld_hive_tool(n));
                crate::core::levenshtein::closest(name, names)
            })
            .map(|s| format!(" — did you mean '{s}'?"))
            .unwrap_or_default();
        Err(ErrorData::new(
            rmcp::model::ErrorCode::METHOD_NOT_FOUND,
            format!("Unknown tool: {name}{suggestion}"),
            None,
        ))
    }

    /// Execute a (synchronous) tool handler on the blocking pool under a
    /// watchdog deadline (#271).
    ///
    /// The handler is `Send + 'static` (it owns an `Arc<dyn McpTool>`, a cloned
    /// arg map and the `ToolContext`), so it runs via `spawn_blocking` on the
    /// dedicated blocking-thread pool. That keeps the few core async workers free
    /// to keep driving the stdio JSON-RPC loop, and — crucially — lets the
    /// watchdog `timeout` actually fire (a synchronous `block_in_place` on the
    /// same task can never be timed out, because the task's own timer cannot be
    /// polled while it blocks).
    async fn run_tool_handler(
        &self,
        name: &str,
        tool: Arc<dyn McpTool>,
        args_map: &serde_json::Map<String, Value>,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let watchdog = Self::handler_watchdog_for(name, args_map);
        let args_owned = args_map.clone();
        let tool_name = name.to_string();
        let agent_id = self
            .agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        // Tokio task locals do not propagate into spawn_blocking. Capture the
        // immutable handoff, then scope only the synchronous handler invocation.
        let handoff = crate::core::context_kernel::bridge::runtime::current_handoff();
        let task = crate::core::task_spine::TaskSpine::current();
        let receipt_capture = crate::server::native_receipts::current_capture();
        let archive_capture = crate::core::archive::authority::current_capture();
        let admissions = crate::core::context_admission::capture::current();
        // The presenter capability the host showed for this call: without it
        // the handler would act as the process's own presenter.
        let presenter = crate::core::archive::presented::request_presenter();
        // The caller's own session for session-continuity reads and writes in
        // the handler (I07): never the project's latest saved session.
        let caller = ctx.session.clone();
        let workspace = crate::core::workspace_authority::current();
        // The user's approval of this ctx_shell call (server::shell_approval).
        let shell_grant = crate::server::shell_approval::current_grant();
        let write_roots: Vec<String> = std::iter::once(ctx.project_root.clone())
            .chain(ctx.extra_roots.iter().cloned())
            .collect();
        let policy_project = crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten()
            .or_else(|| {
                (!ctx.project_root.is_empty()).then(|| std::path::PathBuf::from(&ctx.project_root))
            });
        let join = tokio::task::spawn_blocking(move || {
            let handle = || {
                crate::core::archive::authority::CAPTURE.sync_scope(archive_capture, || {
                    crate::core::task_spine::TaskSpine::sync_scope(task, || {
                        crate::server::native_receipts::NATIVE_RECEIPT_CAPTURE.sync_scope(
                            receipt_capture,
                            || {
                                crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
                                    std::cell::RefCell::new(policy_project),
                                    || {
                                        crate::core::context_kernel::bridge::runtime::KERNEL_PLANNING_HANDOFF
                                            .sync_scope(handoff, || {
                                                crate::core::context_admission::capture::ADMISSIONS
                                                    .sync_scope(admissions, || {
                                                        crate::core::archive::presented::with_presenter(
                                                            presenter,
                                                            || {
                                                                crate::core::pathjail_scope::with_request_write_roots(
                                                                    &write_roots,
                                                                    || {
                                                                        crate::core::session::caller::scope(
                                                                            caller.clone(),
                                                                            || {
                                                                                crate::core::workspace_authority::sync_scope(
                                                                                    workspace.clone(),
                                                                                    || {
                                                                                        crate::server::shell_approval::SHELL_GRANT
                                                                                            .sync_scope(shell_grant.clone(), || {
                                                                                                tool.handle(&args_owned, &ctx)
                                                                                            })
                                                                                    },
                                                                                )
                                                                            },
                                                                        )
                                                                    },
                                                                )
                                                            },
                                                        )
                                                    })
                                            })
                                    },
                                )
                            },
                        )
                    })
                })
            };
            // Security events are counted once per tool result: the collector
            // sees exactly this handler, and the tally is recorded (audit trail +
            // session counters) before `record_call` publishes the value snapshot.
            let (result, tally) = crate::core::security_events::collect_call(handle);
            crate::core::security_events::record_call(&tool_name, &agent_id, &tally);
            result
        });
        Self::watchdog_join(name, join, watchdog).await
    }

    /// [`Self::handler_watchdog`], except that `ctx_agent action=wait` may
    /// block for its own `timeout_secs` (it bounds itself) plus a margin.
    fn handler_watchdog_for(name: &str, args: &serde_json::Map<String, Value>) -> Option<Duration> {
        let base = Self::handler_watchdog(name)?;
        if name == "ctx_agent" && args.get("action").and_then(Value::as_str) == Some("wait") {
            let secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(crate::tools::ctx_agent::WAIT_DEFAULT_SECS)
                .min(crate::tools::ctx_agent::WAIT_MAX_SECS);
            return Some(base.max(Duration::from_secs(secs + 30)));
        }
        Some(base)
    }

    /// Await a blocking handler's join handle, enforcing an optional watchdog.
    ///
    /// On timeout the join handle is **aborted** — the blocking-pool thread
    /// receives a cancellation signal so it does not permanently leak a pool
    /// slot (#1018). A clean error is always returned so the MCP client gets a
    /// JSON-RPC reply. A handler panic is isolated and surfaced as an error too.
    async fn watchdog_join(
        name: &str,
        join: tokio::task::JoinHandle<Result<ToolOutput, ErrorData>>,
        watchdog: Option<Duration>,
    ) -> Result<ToolOutput, ErrorData> {
        let Some(limit) = watchdog else {
            return Self::unwrap_join(name, join.await);
        };
        tokio::select! {
            joined = join => Self::unwrap_join(name, joined),
            () = tokio::time::sleep(limit) => {
                // The JoinHandle was moved into `select!` — when this branch
                // wins, Tokio drops the handle which sets the task's cancelled
                // flag. For `spawn_blocking` this means the thread will be
                // reclaimed once it returns (#1018).
                crate::core::io_health::record_freeze();
                tracing::error!(
                    tool = name,
                    timeout_secs = limit.as_secs(),
                    "tool watchdog fired — handler cancelled to reclaim pool slot (#1018)"
                );
                Err(ErrorData::internal_error(
                    format!(
                        "tool '{name}' exceeded its {}s watchdog and was abandoned. \
                         The MCP server is still running — retry or narrow the request.",
                        limit.as_secs()
                    ),
                    None,
                ))
            }
        }
    }

    /// Collapse a `spawn_blocking` join result into the handler result.
    /// A `JoinError` (handler panic) becomes a clean error instead of crashing
    /// the request task.
    fn unwrap_join(
        name: &str,
        joined: Result<Result<ToolOutput, ErrorData>, tokio::task::JoinError>,
    ) -> Result<ToolOutput, ErrorData> {
        match joined {
            Ok(inner) => inner,
            Err(join_err) => {
                tracing::error!(
                    tool = name,
                    is_panic = join_err.is_panic(),
                    "tool handler did not complete (panic isolated on the blocking pool)"
                );
                Err(ErrorData::internal_error(
                    format!(
                        "tool '{name}' failed unexpectedly. The MCP server is still running \
                         — retry or use a different approach."
                    ),
                    None,
                ))
            }
        }
    }

    /// Watchdog deadline for a single tool handler, or `None` to disable it.
    ///
    /// `ctx_shell` / `ctx_execute` run arbitrary user commands (builds, long
    /// test suites) and already enforce their own command timeouts, so a generic
    /// watchdog would wrongly abort a legitimate long-running command. Every
    /// other tool is bounded so a hang can never swallow the JSON-RPC response.
    /// Tunable via `LEAN_CTX_TOOL_TIMEOUT_SECS` (`0` disables the watchdog).
    fn handler_watchdog(name: &str) -> Option<Duration> {
        if super::is_shell_tool_name(name) {
            return None;
        }
        watchdog_from_secs(read_watchdog_secs())
    }
}

/// Last-pass degenerate-output filter — EXCEPT for protected read tools (#709):
/// their contract is byte-fidelity. File content is never a model artifact, so
/// a file that legitimately contains flood-like lines (`!!!!!!!!!!` in test
/// fixtures, ASCII art) must survive a `mode=raw`/`full` read byte-for-byte.
/// Every other tool keeps the #257 degenerate-CJK/flood cleanup, whose target
/// is compressed/summarized output.
fn sanitized_tool_text(name: &str, raw_text: String) -> String {
    if crate::core::firewall::is_protected_read(name) {
        return raw_text;
    }
    crate::core::output_sanitizer::sanitize(&raw_text)
}

/// Read the configured watchdog budget in seconds (defaults to
/// [`DEFAULT_TOOL_TIMEOUT_SECS`]). Kept separate from the policy so the pure
/// `secs -> Option<Duration>` mapping stays trivially testable.
fn read_watchdog_secs() -> u64 {
    std::env::var("LEAN_CTX_TOOL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
}

/// Map a watchdog budget in seconds to a duration; `0` disables the watchdog.
fn watchdog_from_secs(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

const REFERENCE_THRESHOLD: usize = 4000;

/// Default per-tool watchdog budget (#271). Long enough that no legitimate
/// read/search/graph call ever hits it, short enough that a hang degrades to a
/// clean error instead of a dropped request.
const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 120;

use super::tool_trait::PATH_LIKE_KEYS;

/// Spellings agents carry over from other tool sets (Claude Code's
/// `file_path`, shell tools' `cmd`) for a parameter a tool calls differently.
const ARG_ALIASES: &[(&str, &[&str])] = &[
    ("path", &["file_path", "filePath", "file", "filename"]),
    ("command", &["cmd"]),
];

/// Renames an alias to the canonical parameter when the tool declares the
/// canonical name but not the alias, and the caller sent only the alias. A
/// tool that declares the alias itself keeps its own meaning for it, and an
/// explicit canonical value always wins. `None` when nothing changed, so the
/// common call borrows the original map.
pub(crate) fn canonicalize_arg_aliases(
    tool: &dyn McpTool,
    args: &serde_json::Map<String, Value>,
) -> Option<serde_json::Map<String, Value>> {
    let pending: Vec<(&str, &str)> = ARG_ALIASES
        .iter()
        .filter(|(canonical, _)| !args.contains_key(*canonical))
        .filter_map(|(canonical, aliases)| {
            aliases
                .iter()
                .find(|alias| args.get(**alias).is_some_and(Value::is_string))
                .map(|alias| (*canonical, *alias))
        })
        .collect();
    if pending.is_empty() {
        return None;
    }
    let def = tool.tool_def();
    let declared = def
        .input_schema
        .get("properties")
        .and_then(Value::as_object)?;
    let mut out = args.clone();
    let mut changed = false;
    for (canonical, alias) in pending {
        if declared.contains_key(canonical)
            && !declared.contains_key(alias)
            && let Some(value) = out.remove(alias)
        {
            out.insert(canonical.to_string(), value);
            changed = true;
        }
    }
    changed.then_some(out)
}

#[cfg(test)]
mod alias_tests {
    use super::*;

    fn tool(name: &str) -> Arc<dyn McpTool> {
        crate::server::registry::build_registry()
            .get_arc(name)
            .unwrap_or_else(|| panic!("{name} registered"))
    }

    fn map(v: Value) -> serde_json::Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("expected an object, got {other}"),
        }
    }

    #[test]
    fn file_path_becomes_path_for_ctx_read() {
        let out = canonicalize_arg_aliases(
            tool("ctx_read").as_ref(),
            &map(serde_json::json!({"file_path": "src/main.rs", "mode": "map"})),
        )
        .expect("alias renamed");
        assert_eq!(out.get("path").and_then(Value::as_str), Some("src/main.rs"));
        assert!(!out.contains_key("file_path"));
    }

    #[test]
    fn cmd_becomes_command_for_ctx_shell() {
        let out = canonicalize_arg_aliases(
            tool("ctx_shell").as_ref(),
            &map(serde_json::json!({"cmd": "git status"})),
        )
        .expect("alias renamed");
        assert_eq!(
            out.get("command").and_then(Value::as_str),
            Some("git status")
        );
    }

    #[test]
    fn declared_alias_and_explicit_canonical_are_untouched() {
        // ctx_search declares `file_path` itself (#846).
        assert!(
            canonicalize_arg_aliases(
                tool("ctx_search").as_ref(),
                &map(serde_json::json!({"pattern": "x", "file_path": "a.rs"})),
            )
            .is_none()
        );
        assert!(
            canonicalize_arg_aliases(
                tool("ctx_read").as_ref(),
                &map(serde_json::json!({"path": "a.rs", "file_path": "b.rs"})),
            )
            .is_none()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_snapshot_uses_normal_pipeline_and_token_accounting() {
        let server = crate::tools::create_server();
        let args = serde_json::json!({
            "action": "query", "mode": "snapshot", "provider": "gitlab",
            "resource": "merge_requests", "limit": 0
        });
        let (text, saved, _, blocks) = server
            .dispatch_inner("ctx_provider", args.as_object(), false)
            .await
            .expect("bounded input error is a snapshot envelope");
        assert!(
            blocks.is_none(),
            "snapshot must not bypass policy/budget/IR processing"
        );
        assert_eq!(saved, 0);
        let parsed: Value = serde_json::from_str(&text).expect("unmodified JSON");
        assert_eq!(parsed["schema_version"], 1);
        assert!(parsed.get("error").is_some());
        let calls = server.tool_calls.read().await;
        let record = calls.last().expect("snapshot call recorded");
        assert_eq!(record.mode.as_deref(), Some("snapshot"));
        assert_eq!(
            record.original_tokens,
            crate::core::tokens::count_tokens(&text)
        );
        assert!(record.original_tokens > 0);
        assert_eq!(record.saved_tokens, 0);
    }

    #[test]
    fn protected_reads_bypass_the_degenerate_output_sanitizer() {
        // #709 follow-up: an explicit read is byte-exact even for content that
        // *looks* like degenerate model output (fixtures full of `!!!!!!!!!!`).
        let fixture = "ok line\n!!!!!!!!!!!!\ntail\n".to_string();
        assert_eq!(
            sanitized_tool_text("ctx_read", fixture.clone()),
            fixture,
            "ctx_read must never lose file bytes to the flood filter"
        );

        // Non-read tools keep the #257 degenerate-output cleanup.
        let cleaned = sanitized_tool_text("ctx_shell", fixture);
        assert!(
            !cleaned.contains("!!!!!!!!!!"),
            "flood line must be removed"
        );
        assert!(cleaned.contains("ok line") && cleaned.contains("tail"));
    }

    #[test]
    fn path_like_keys_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for key in PATH_LIKE_KEYS {
            assert!(seen.insert(*key), "duplicate PATH_LIKE_KEYS entry: {key}");
        }
    }

    #[test]
    fn path_like_keys_includes_primary_keys() {
        for primary in &["path", "project_root", "root"] {
            assert!(
                PATH_LIKE_KEYS.contains(primary),
                "primary key '{primary}' missing from PATH_LIKE_KEYS"
            );
        }
    }

    #[test]
    fn path_like_keys_all_non_empty() {
        for key in PATH_LIKE_KEYS {
            assert!(!key.is_empty(), "PATH_LIKE_KEYS contains empty string");
        }
        assert!(
            PATH_LIKE_KEYS.len() >= 3,
            "PATH_LIKE_KEYS must have at least the 3 primary keys"
        );
    }

    #[test]
    fn watchdog_disabled_for_long_running_shell_tools() {
        assert!(
            LeanCtxServer::handler_watchdog("ctx_shell").is_none(),
            "ctx_shell runs arbitrary user commands and must not be watchdog-bounded"
        );
        assert!(
            LeanCtxServer::handler_watchdog("ctx_execute").is_none(),
            "ctx_execute must not be watchdog-bounded"
        );
    }

    #[test]
    fn ctx_agent_wait_outlives_the_default_watchdog_by_its_own_timeout() {
        let args = |v: Value| v.as_object().cloned().unwrap();
        if std::env::var_os("LEAN_CTX_TOOL_TIMEOUT_SECS").is_some() {
            return;
        }
        let wait = LeanCtxServer::handler_watchdog_for(
            "ctx_agent",
            &args(serde_json::json!({"action": "wait", "timeout_secs": 900})),
        );
        assert_eq!(wait, Some(Duration::from_secs(930)));
        let default_wait = LeanCtxServer::handler_watchdog_for(
            "ctx_agent",
            &args(serde_json::json!({"action": "wait"})),
        );
        assert_eq!(default_wait, Some(Duration::from_secs(630)));
        let read = LeanCtxServer::handler_watchdog_for(
            "ctx_agent",
            &args(serde_json::json!({"action": "read"})),
        );
        assert_eq!(read, Some(Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS)));
    }

    #[test]
    fn watchdog_from_secs_zero_disables() {
        assert!(watchdog_from_secs(0).is_none());
    }

    #[test]
    fn watchdog_from_secs_positive_maps_to_duration() {
        assert_eq!(watchdog_from_secs(5).unwrap().as_secs(), 5);
        assert_eq!(
            watchdog_from_secs(DEFAULT_TOOL_TIMEOUT_SECS)
                .unwrap()
                .as_secs(),
            120
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_returns_error_on_hung_handler() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // A hung handler must surface as a clean error (not a dropped request),
        // which is the core #271 guarantee. The stop flag lets the simulated
        // hang exit promptly after the assertion so the runtime shuts down fast.
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_in = stop.clone();
        let join = tokio::task::spawn_blocking(move || {
            for _ in 0..400 {
                if stop_in.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(ToolOutput::simple("late".to_string()))
        });
        let start = std::time::Instant::now();
        let result =
            LeanCtxServer::watchdog_join("mock", join, Some(Duration::from_millis(200))).await;
        let elapsed = start.elapsed();
        stop.store(true, Ordering::Relaxed);
        assert!(result.is_err(), "hung handler must surface as an error");
        assert!(
            elapsed < Duration::from_secs(2),
            "watchdog must fire promptly, took {elapsed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_passes_through_fast_handler() {
        let join = tokio::task::spawn_blocking(|| Ok(ToolOutput::simple("ok".to_string())));
        let result = LeanCtxServer::watchdog_join("mock", join, Some(Duration::from_secs(5))).await;
        assert_eq!(result.unwrap().text, "ok");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_none_awaits_handler_to_completion() {
        let join = tokio::task::spawn_blocking(|| Ok(ToolOutput::simple("ok".to_string())));
        let result = LeanCtxServer::watchdog_join("ctx_shell", join, None).await;
        assert_eq!(result.unwrap().text, "ok");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_panic_becomes_error_not_crash() {
        let join: tokio::task::JoinHandle<Result<ToolOutput, ErrorData>> =
            tokio::task::spawn_blocking(|| panic!("simulated handler panic"));
        let result = LeanCtxServer::watchdog_join("mock", join, Some(Duration::from_secs(5))).await;
        assert!(
            result.is_err(),
            "a handler panic must surface as an error; the server process survives"
        );
    }

    /// #712: a typo'd tool name must return a "did you mean" hint from the
    /// live registry instead of a bare unknown-tool error; a name nowhere
    /// near any registered tool must stay suggestion-free.
    /// C8: a shared server never runs ctx_tasks for its clients.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shared_server_refuses_hive_tasks() {
        if crate::test_env::run_in_own_process(
            "server::dispatch::tests::a_shared_server_refuses_hive_tasks",
            &[(crate::core::hive::PREVIEW_ENV, "1")],
        ) {
            return;
        }
        let mut server = crate::tools::create_server();
        server.session_mode = crate::tools::SessionMode::Shared;
        let mut args = serde_json::Map::new();
        args.insert("action".into(), Value::String("list".into()));
        let (text, ..) = server
            .dispatch_tool("ctx_tasks", Some(&args), false)
            .await
            .unwrap();
        assert!(text.contains("only in a personal MCP session"), "{text}");
    }

    /// Owner 28.10.: while the Hive switch is off, ctx_tasks is in no tool
    /// list and every call path answers one fixed line.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hive_tools_are_withheld_while_the_switch_is_off() {
        if crate::core::hive::available_locally() {
            return;
        }
        // The guarded calls persist receipts: alone, in a store of its own, so
        // the suite's shared store under load cannot withhold the answer.
        let store = tempfile::tempdir().unwrap();
        let store = store.path().to_str().unwrap();
        if crate::test_env::run_in_own_process(
            "server::dispatch::tests::hive_tools_are_withheld_while_the_switch_is_off",
            &[
                ("LEAN_CTX_DATA_DIR", store),
                ("LEAN_CTX_CONFIG_DIR", store),
                ("LEAN_CTX_STATE_DIR", store),
                ("LEAN_CTX_CACHE_DIR", store),
            ],
        ) {
            return;
        }
        use crate::core::tool_profiles::ToolProfile;
        use crate::server::dynamic_tools::HIVE_TOOLS;
        let registry = crate::server::registry::build_registry();
        let mut surfaces = vec![registry.tool_defs(), registry.active_tool_defs()];
        for profile in [
            ToolProfile::Minimal,
            ToolProfile::Standard,
            ToolProfile::Power,
        ] {
            surfaces.push(registry.profile_tool_defs(&profile));
        }
        for defs in &surfaces {
            for def in defs {
                assert!(!HIVE_TOOLS.contains(&def.name.as_ref()), "{}", def.name);
            }
        }
        for name in HIVE_TOOLS {
            assert!(
                !crate::server::tool_visibility::is_tool_visible(
                    name,
                    &ToolProfile::Power,
                    &[],
                    crate::server::tool_visibility::ClientQuirks::default(),
                    true,
                ),
                "{name}"
            );
            // The answer may echo the query ("No tools found matching '…'"); no listed entry
            // may be the withheld tool.
            let found = crate::tool_defs::discover_tools(name);
            assert!(
                !found
                    .lines()
                    .any(|l| l.trim_start().starts_with(&format!("{name} —"))),
                "{found}"
            );
        }

        let server = crate::tools::create_server();
        let line = crate::core::hive::TOOL_NOT_AVAILABLE;
        for name in HIVE_TOOLS {
            let mut args = serde_json::Map::new();
            args.insert("action".into(), Value::String("list".into()));
            let (direct, ..) = server
                .dispatch_tool(name, Some(&args), false)
                .await
                .unwrap();
            assert_eq!(direct, line);

            let mut wrapped = serde_json::Map::new();
            wrapped.insert("name".into(), Value::String((*name).into()));
            wrapped.insert("arguments".into(), Value::Object(args.clone()));
            let (via_call, ..) = server
                .dispatch_tool("ctx_call", Some(&wrapped), false)
                .await
                .unwrap();
            assert_eq!(via_call, line);

            for (outer, outer_args) in [
                (*name, args.clone()),
                ("ctx_call", wrapped.clone()),
                ("ctx", {
                    let mut meta = args.clone();
                    meta.insert("tool".into(), Value::String((*name).into()));
                    meta
                }),
            ] {
                let request =
                    rmcp::model::CallToolRequestParams::new(outer).with_arguments(outer_args);
                let result = server.call_tool_guarded(request).await.unwrap();
                assert_eq!(result.is_error, Some(true), "{outer}");
                let text: Vec<_> = result
                    .content
                    .iter()
                    .filter_map(|block| block.as_text())
                    .map(|block| block.text.as_str())
                    .collect();
                assert_eq!(text, [line], "{outer}");
            }
        }
    }

    /// With the test preview the Hive tools are advertised as before.
    #[test]
    fn hive_tools_return_with_the_preview() {
        if crate::test_env::run_in_own_process(
            "server::dispatch::tests::hive_tools_return_with_the_preview",
            &[(crate::core::hive::PREVIEW_ENV, "1")],
        ) {
            return;
        }
        let defs = crate::server::registry::build_registry().tool_defs();
        for name in crate::server::dynamic_tools::HIVE_TOOLS {
            assert!(defs.iter().any(|def| def.name.as_ref() == *name), "{name}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_tool_suggests_closest_registered_name() {
        let server = crate::tools::create_server();

        let err = server
            .dispatch_tool("ctx_raed", None, false)
            .await
            .expect_err("typo'd tool must error");
        assert!(
            err.message.contains("Unknown tool: ctx_raed"),
            "unexpected message: {}",
            err.message
        );
        assert!(
            err.message.contains("did you mean 'ctx_read'"),
            "missing suggestion: {}",
            err.message
        );

        // A removed tool names its successor instead of a near-miss guess.
        let err = server
            .dispatch_tool("ctx_overview", None, false)
            .await
            .expect_err("removed tool must error");
        assert!(
            err.message
                .starts_with("ctx_overview was removed; use ctx_compose instead"),
            "unexpected message: {}",
            err.message
        );

        let err = server
            .dispatch_tool("zzz_qqq_www", None, false)
            .await
            .expect_err("unrelated name must error");
        assert!(
            !err.message.contains("did you mean"),
            "no confident suggestion for garbage: {}",
            err.message
        );
    }

    /// #1604: `ctx_call(name="ctx_edit", path=…, old_string=…)` — the inner
    /// tool's arguments flattened onto the ctx_call envelope instead of nested
    /// under `arguments` — used to reach the inner tool with NO arguments, so
    /// it answered "path is required" about a parameter the caller had just
    /// supplied. `name` is the only key ctx_call reserves, so the rest is
    /// forwarded. The nested form and the #658 misspelling guard are unchanged.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ctx_call_forwards_flattened_inner_arguments() {
        let server = crate::tools::create_server();
        fn msg(
            r: Result<
                (
                    String,
                    usize,
                    Option<ShellOutcome>,
                    Option<Vec<ContentBlock>>,
                ),
                ErrorData,
            >,
        ) -> String {
            match r {
                Ok((text, ..)) => text,
                Err(e) => e.message.to_string(),
            }
        }

        // `ctx_read` on purpose: it is on every built-in role's allowlist, so a
        // sibling test that leaves a non-default role in the global slot cannot
        // turn these assertions into `[ROLE DENIED]`. Its missing-argument error
        // is also the exact one from the report.
        //
        // Control: genuinely no arguments — the inner tool's own error stands.
        let mut bare = serde_json::Map::new();
        bare.insert("name".into(), Value::String("ctx_read".into()));
        let bare = msg(server.dispatch_tool("ctx_call", Some(&bare), false).await);
        assert!(
            bare.contains("path is required"),
            "an argument-less inner call must still say what is missing: {bare}"
        );

        // Flattened: the same message would be a lie — the caller passed it.
        let mut flat = serde_json::Map::new();
        flat.insert("name".into(), Value::String("ctx_read".into()));
        flat.insert("path".into(), Value::String("Cargo.toml".into()));
        let flat = msg(server.dispatch_tool("ctx_call", Some(&flat), false).await);
        assert!(
            !flat.contains("path is required"),
            "flattened arguments must reach the inner tool: {flat}"
        );

        // Nested form: unchanged.
        let mut nested = serde_json::Map::new();
        nested.insert("name".into(), Value::String("ctx_read".into()));
        nested.insert(
            "arguments".into(),
            serde_json::json!({ "path": "Cargo.toml" }),
        );
        let nested = msg(server.dispatch_tool("ctx_call", Some(&nested), false).await);
        assert!(
            !nested.contains("path is required"),
            "nested arguments must keep working: {nested}"
        );

        // #658 guard: an `args`/`params` wrapper is a typo, not a flat call —
        // forwarding it would hand the inner tool a bogus `args` parameter.
        let mut typo = serde_json::Map::new();
        typo.insert("name".into(), Value::String("ctx_read".into()));
        typo.insert("args".into(), serde_json::json!({ "path": "Cargo.toml" }));
        let typo = msg(server.dispatch_tool("ctx_call", Some(&typo), false).await);
        assert!(
            typo.contains("unknown key 'args'"),
            "the #658 misspelling hint must survive: {typo}"
        );
    }
}

#[cfg(test)]
mod workflow_gate_tests {
    use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
    use crate::core::policy::runtime::TestPolicyOverride;
    use crate::core::workflow::gate::tests::{review, workflow_file};
    use crate::core::workflow::{is_withheld_sentinel, load_active, save_active};
    use serde_json::{Map, Value, json};

    fn call(inner: &str, arguments: &Value) -> Map<String, Value> {
        let mut args = Map::new();
        args.insert("name".into(), Value::String(inner.into()));
        args.insert("arguments".into(), arguments.clone());
        args
    }

    /// The `ctx_call` gate under the stand-in for a withheld workflow: the
    /// same closed decision as the direct gate — pass-through tools and write
    /// actions refused, the control tool working, no expiry, the stored
    /// workflow untouched — and the real workflow back once it may be read.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_ctx_call_gate_keeps_a_withheld_workflow_closed() {
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other) = project();
        scoped(&root, || save_active(&review())).expect("saved in its project");
        let stored = std::fs::read(workflow_file()).expect("stored workflow");

        let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&other));
        let mut closed = scoped(&other, load_active)
            .expect("load")
            .expect("a withheld workflow is still a workflow");
        assert!(is_withheld_sentinel(&closed));
        closed.updated_at = chrono::Utc::now() - chrono::Duration::minutes(45);
        *server.workflow.write().await = Some(closed);

        for (inner, arguments) in [
            ("ctx_read", json!({ "path": "Cargo.toml" })),
            (
                "ctx_session",
                json!({ "action": "finding", "value": "noted" }),
            ),
            (
                "ctx_session",
                json!({ "action": "decision", "value": "decided" }),
            ),
        ] {
            let (reply, ..) = server
                .dispatch_tool("ctx_call", Some(&call(inner, &arguments)), false)
                .await
                .expect("a refusal is a reply");
            assert!(
                reply.contains("withheld by the context gateway"),
                "{inner}: {reply}"
            );
            assert!(!reply.contains("review"), "{reply}");
        }
        assert!(
            server
                .workflow
                .read()
                .await
                .as_ref()
                .is_some_and(is_withheld_sentinel),
            "not cleared by its age"
        );

        assert_eq!(
            std::fs::read(workflow_file()).expect("kept"),
            stored,
            "the withheld workflow is byte-identical on disk"
        );

        // The authority holds again: the real workflow loads, no stand-in.
        let real = scoped(&root, load_active).expect("load").expect("active");
        assert!(!is_withheld_sentinel(&real));
        *server.workflow.write().await = Some(real);
        let (reply, ..) = server
            .dispatch_tool(
                "ctx_call",
                Some(&call("ctx_shell", &json!({ "command": "true" }))),
                false,
            )
            .await
            .expect("a refusal is a reply");
        assert!(reply.contains("blocked by workflow 'review'"), "{reply}");
    }
}
